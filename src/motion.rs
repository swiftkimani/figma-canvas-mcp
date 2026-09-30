//! Prototype interactions and motion, normalized into something emittable.
//!
//! Figma's motion model is richer than CSS's. Three cases matter:
//!
//! * **Named easings** map onto CSS keywords closely enough to be exact.
//! * **Custom cubic beziers** map exactly — same four control points.
//! * **Springs** have no bezier equivalent at all. Rather than approximate one,
//!   we simulate the damped oscillator and emit a CSS `linear()` stop list, which
//!   reproduces overshoot faithfully. The raw spring parameters are kept too, so a
//!   motion library can use them directly.

use serde::{Deserialize, Serialize};

use crate::raw::{RawAction, RawEasing, RawReaction, RawTransition, RawTrigger};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Click,
    Hover,
    Press,
    Drag,
    MouseEnter,
    MouseLeave,
    MouseDown,
    MouseUp,
    KeyDown,
    AfterTimeout,
    Other,
}

impl Trigger {
    fn parse(s: &str) -> Trigger {
        match s {
            "ON_CLICK" => Trigger::Click,
            "ON_HOVER" => Trigger::Hover,
            "ON_PRESS" => Trigger::Press,
            "ON_DRAG" => Trigger::Drag,
            "MOUSE_ENTER" => Trigger::MouseEnter,
            "MOUSE_LEAVE" => Trigger::MouseLeave,
            "MOUSE_DOWN" => Trigger::MouseDown,
            "MOUSE_UP" => Trigger::MouseUp,
            "ON_KEY_DOWN" => Trigger::KeyDown,
            "AFTER_TIMEOUT" => Trigger::AfterTimeout,
            _ => Trigger::Other,
        }
    }

    /// The CSS pseudo-class this trigger becomes, when it has one.
    ///
    /// Hover and press are expressible in pure CSS, which is why they are worth
    /// special-casing: the generated component needs no JavaScript for them.
    pub fn pseudo_class(self) -> Option<&'static str> {
        match self {
            Trigger::Hover | Trigger::MouseEnter => Some(":hover"),
            Trigger::Press | Trigger::MouseDown => Some(":active"),
            _ => None,
        }
    }

    /// The React handler prop this trigger becomes, when CSS cannot express it.
    pub fn handler(self) -> Option<&'static str> {
        match self {
            Trigger::Click | Trigger::MouseUp => Some("onClick"),
            Trigger::KeyDown => Some("onKeyDown"),
            Trigger::Drag => Some("onDragStart"),
            Trigger::MouseLeave => Some("onMouseLeave"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Go to another frame — a route in code.
    Navigate {
        destination_id: Option<String>,
        destination_name: Option<String>,
    },
    /// Swap this instance for another variant — a state change.
    ChangeTo {
        destination_id: Option<String>,
        destination_name: Option<String>,
    },
    OpenOverlay {
        destination_name: Option<String>,
    },
    CloseOverlay,
    Back,
    OpenUrl {
        url: String,
    },
    ScrollTo {
        destination_name: Option<String>,
    },
    SetVariable {
        variable_name: Option<String>,
    },
    Other {
        raw: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Spring {
    pub mass: f64,
    pub stiffness: f64,
    pub damping: f64,
    pub initial_velocity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    /// Figma's transition type, verbatim.
    pub kind: String,
    pub duration_ms: f64,
    /// A CSS `transition-timing-function` value: a keyword, `cubic-bezier(...)`,
    /// or a `linear(...)` stop list for springs.
    pub easing_css: String,
    /// Figma's easing name, kept so a consumer can do better than our mapping.
    pub easing_kind: String,
    /// True when `easing_css` is an approximation rather than an exact match.
    pub easing_approximate: bool,
    /// Exact spring parameters, for motion libraries that model springs natively.
    pub spring: Option<Spring>,
    pub direction: Option<String>,
    /// Smart Animate tweens matched layers rather than cross-fading.
    pub smart_animate: bool,
}

impl Transition {
    /// A ready-to-use `transition` shorthand value.
    pub fn css_shorthand(&self, property: &str) -> String {
        format!(
            "{property} {}ms {}",
            round(self.duration_ms),
            self.easing_css
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Interaction {
    pub trigger: Trigger,
    /// Figma's trigger name, verbatim.
    pub trigger_kind: String,
    /// Delay before the action fires, in ms.
    pub delay_ms: Option<f64>,
    pub action: Action,
    pub transition: Option<Transition>,
}

fn round(v: f64) -> String {
    if (v - v.round()).abs() < 0.01 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

/// CSS's named keywords are defined as these exact beziers, and Figma's
/// corresponding curves match them closely enough to use the keyword.
fn named_easing(kind: &str) -> Option<(&'static str, bool)> {
    Some(match kind {
        "LINEAR" => ("linear", false),
        "EASE_IN" => ("ease-in", false),
        "EASE_OUT" => ("ease-out", false),
        "EASE_IN_AND_OUT" => ("ease-in-out", false),
        // Figma's overshoot curves have no CSS keyword. These reproduce the
        // shape but are not guaranteed to match Figma's exact control points,
        // so they are flagged as approximate.
        "EASE_IN_BACK" => ("cubic-bezier(0.3, -0.05, 0.7, -0.5)", true),
        "EASE_OUT_BACK" => ("cubic-bezier(0.45, 1.45, 0.8, 1)", true),
        "EASE_IN_AND_OUT_BACK" => ("cubic-bezier(0.7, -0.4, 0.4, 1.4)", true),
        _ => return None,
    })
}

/// Simulate a damped spring and emit a CSS `linear()` stop list.
///
/// `linear()` distributes its values evenly across the duration, and values may
/// exceed 1, which is exactly what overshoot needs. This is why springs come out
/// right instead of being flattened into an ease-out.
pub fn spring_to_linear(s: &Spring, duration_ms: f64, samples: usize) -> String {
    let mass = if s.mass > 0.0 { s.mass } else { 1.0 };
    let samples = samples.clamp(4, 60);

    // x'' = (-k * (x - 1) - c * x') / m, settling at x = 1.
    let mut x = 0.0f64;
    let mut v = s.initial_velocity;
    let total = (duration_ms / 1000.0).max(0.001);
    let steps = 2000;
    let dt = total / steps as f64;

    let mut trace = Vec::with_capacity(steps + 1);
    trace.push(x);
    for _ in 0..steps {
        let a = (-s.stiffness * (x - 1.0) - s.damping * v) / mass;
        v += a * dt;
        x += v * dt;
        trace.push(x);
    }

    let mut stops = Vec::with_capacity(samples);
    for i in 0..samples {
        let t = i as f64 / (samples - 1) as f64;
        let idx = ((t * steps as f64).round() as usize).min(steps);
        stops.push(format!("{:.4}", trace[idx]));
    }
    // Land exactly on the target rather than wherever the simulation drifted to.
    if let Some(last) = stops.last_mut() {
        *last = "1".to_string();
    }
    format!("linear({})", stops.join(", "))
}

fn read_easing(e: Option<&RawEasing>, duration_ms: f64) -> (String, String, bool, Option<Spring>) {
    let Some(e) = e else {
        return ("ease".into(), "UNSPECIFIED".into(), true, None);
    };

    if e.kind == "CUSTOM_CUBIC_BEZIER"
        && let Some(b) = &e.cubic_bezier
    {
        return (
            format!(
                "cubic-bezier({:.4}, {:.4}, {:.4}, {:.4})",
                b.x1, b.y1, b.x2, b.y2
            ),
            e.kind.clone(),
            false,
            None,
        );
    }

    if e.kind == "CUSTOM_SPRING"
        && let Some(sp) = &e.spring
    {
        let spring = Spring {
            mass: sp.mass,
            stiffness: sp.stiffness,
            damping: sp.damping,
            initial_velocity: sp.initial_velocity,
        };
        return (
            spring_to_linear(&spring, duration_ms, 24),
            e.kind.clone(),
            false,
            Some(spring),
        );
    }

    match named_easing(&e.kind) {
        Some((css, approx)) => (css.to_string(), e.kind.clone(), approx, None),
        None => ("ease".into(), e.kind.clone(), true, None),
    }
}

fn read_transition(t: &RawTransition) -> Transition {
    // Figma stores seconds; CSS wants ms.
    let duration_ms = t.duration.unwrap_or(0.3) * 1000.0;
    let (easing_css, easing_kind, easing_approximate, spring) =
        read_easing(t.easing.as_ref(), duration_ms);

    Transition {
        smart_animate: t.kind == "SMART_ANIMATE" || t.match_layers.unwrap_or(false),
        kind: t.kind.clone(),
        duration_ms,
        easing_css,
        easing_kind,
        easing_approximate,
        spring,
        direction: t.direction.clone(),
    }
}

fn read_action(a: &RawAction) -> Action {
    match a.kind.as_str() {
        "URL" => Action::OpenUrl {
            url: a.url.clone().unwrap_or_default(),
        },
        "BACK" => Action::Back,
        "CLOSE" => Action::CloseOverlay,
        "OPEN_OVERLAY" | "SWAP_OVERLAY" => Action::OpenOverlay {
            destination_name: a.destination_name.clone(),
        },
        "SCROLL_TO" => Action::ScrollTo {
            destination_name: a.destination_name.clone(),
        },
        "SET_VARIABLE" => Action::SetVariable {
            variable_name: a.variable_name.clone(),
        },
        "NODE" => {
            // CHANGE_TO is a variant swap — a state change, not a route.
            if a.navigation.as_deref() == Some("CHANGE_TO") {
                Action::ChangeTo {
                    destination_id: a.destination_id.clone(),
                    destination_name: a.destination_name.clone(),
                }
            } else {
                Action::Navigate {
                    destination_id: a.destination_id.clone(),
                    destination_name: a.destination_name.clone(),
                }
            }
        }
        other => Action::Other {
            raw: other.to_string(),
        },
    }
}

pub fn read_reactions(reactions: &[RawReaction]) -> Vec<Interaction> {
    let mut out = Vec::new();
    for r in reactions {
        let (trigger, trigger_kind, delay_ms) = match &r.trigger {
            Some(t) => (Trigger::parse(&t.kind), t.kind.clone(), trigger_delay_ms(t)),
            None => (Trigger::Other, String::new(), None),
        };
        for a in &r.actions {
            out.push(Interaction {
                trigger,
                trigger_kind: trigger_kind.clone(),
                delay_ms,
                action: read_action(a),
                transition: a.transition.as_ref().map(read_transition),
            });
        }
    }
    out
}

fn trigger_delay_ms(t: &RawTrigger) -> Option<f64> {
    t.timeout.or(t.delay).map(|s| s * 1000.0)
}

/// The transition to use for CSS state styling, if this node has one.
///
/// Only hover and press are pure CSS, so those are the ones worth lifting into a
/// `transition` declaration on the base rule.
pub fn css_state_transition(interactions: &[Interaction]) -> Option<&Transition> {
    interactions
        .iter()
        .find(|i| i.trigger.pseudo_class().is_some() && i.transition.is_some())
        .and_then(|i| i.transition.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::{RawCubicBezier, RawSpring as RS};

    fn reaction(trigger: &str, action: RawAction) -> RawReaction {
        RawReaction {
            trigger: Some(RawTrigger {
                kind: trigger.into(),
                ..Default::default()
            }),
            actions: vec![action],
        }
    }

    #[test]
    fn hover_becomes_a_css_pseudo_class_not_a_handler() {
        assert_eq!(Trigger::Hover.pseudo_class(), Some(":hover"));
        assert_eq!(Trigger::Hover.handler(), None);
        assert_eq!(Trigger::Click.handler(), Some("onClick"));
        assert_eq!(Trigger::Click.pseudo_class(), None);
    }

    #[test]
    fn seconds_become_milliseconds() {
        let r = reaction(
            "ON_CLICK",
            RawAction {
                kind: "NODE".into(),
                destination_id: Some("2:1".into()),
                destination_name: Some("Detail screen".into()),
                navigation: Some("NAVIGATE".into()),
                transition: Some(RawTransition {
                    kind: "SMART_ANIMATE".into(),
                    duration: Some(0.45),
                    easing: Some(RawEasing {
                        kind: "EASE_OUT".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let i = &read_reactions(&[r])[0];
        let t = i.transition.as_ref().unwrap();
        assert_eq!(t.duration_ms, 450.0);
        assert_eq!(t.easing_css, "ease-out");
        assert!(!t.easing_approximate);
        assert!(t.smart_animate);
        assert_eq!(t.css_shorthand("all"), "all 450ms ease-out");
        assert!(
            matches!(&i.action, Action::Navigate { destination_name, .. }
            if destination_name.as_deref() == Some("Detail screen"))
        );
    }

    #[test]
    fn custom_bezier_is_exact_not_approximated() {
        let r = reaction(
            "ON_HOVER",
            RawAction {
                kind: "NODE".into(),
                navigation: Some("CHANGE_TO".into()),
                transition: Some(RawTransition {
                    kind: "SMART_ANIMATE".into(),
                    duration: Some(0.2),
                    easing: Some(RawEasing {
                        kind: "CUSTOM_CUBIC_BEZIER".into(),
                        cubic_bezier: Some(RawCubicBezier {
                            x1: 0.17,
                            y1: 0.67,
                            x2: 0.83,
                            y2: 0.67,
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let i = &read_reactions(&[r])[0];
        let t = i.transition.as_ref().unwrap();
        assert_eq!(t.easing_css, "cubic-bezier(0.1700, 0.6700, 0.8300, 0.6700)");
        assert!(
            !t.easing_approximate,
            "an exact bezier is not an approximation"
        );
        // A variant swap is a state change, not a route.
        assert!(matches!(i.action, Action::ChangeTo { .. }));
    }

    #[test]
    fn a_spring_becomes_a_linear_stop_list_that_overshoots() {
        let r = reaction(
            "ON_CLICK",
            RawAction {
                kind: "NODE".into(),
                transition: Some(RawTransition {
                    kind: "SMART_ANIMATE".into(),
                    duration: Some(0.8),
                    easing: Some(RawEasing {
                        kind: "CUSTOM_SPRING".into(),
                        // Underdamped, so it must overshoot past 1.
                        spring: Some(RS {
                            mass: 1.0,
                            stiffness: 200.0,
                            damping: 8.0,
                            initial_velocity: 0.0,
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let i = &read_reactions(&[r])[0];
        let t = i.transition.as_ref().unwrap();

        assert!(t.easing_css.starts_with("linear("), "got {}", t.easing_css);
        assert!(!t.easing_approximate, "a simulated spring is not a guess");
        assert!(
            t.spring.is_some(),
            "raw params must survive for motion libraries"
        );

        let values: Vec<f64> = t
            .easing_css
            .trim_start_matches("linear(")
            .trim_end_matches(')')
            .split(", ")
            .map(|v| v.parse().unwrap())
            .collect();
        assert_eq!(values[0], 0.0, "must start at rest");
        assert_eq!(*values.last().unwrap(), 1.0, "must land exactly on target");
        assert!(
            values.iter().any(|v| *v > 1.02),
            "an underdamped spring should overshoot: {values:?}"
        );
    }

    #[test]
    fn overdamped_spring_does_not_overshoot() {
        let spring = Spring {
            mass: 1.0,
            stiffness: 100.0,
            damping: 40.0,
            initial_velocity: 0.0,
        };
        let css = spring_to_linear(&spring, 600.0, 20);
        let values: Vec<f64> = css
            .trim_start_matches("linear(")
            .trim_end_matches(')')
            .split(", ")
            .map(|v| v.parse().unwrap())
            .collect();
        assert!(
            values.iter().all(|v| *v <= 1.001),
            "heavily damped spring must not overshoot: {values:?}"
        );
    }

    #[test]
    fn back_easings_are_flagged_as_approximate() {
        let (css, approx) = named_easing("EASE_OUT_BACK").unwrap();
        assert!(css.starts_with("cubic-bezier("));
        assert!(
            approx,
            "Figma's exact BACK control points are not published"
        );
    }

    #[test]
    fn only_css_expressible_triggers_lift_into_a_transition_declaration() {
        let hover = reaction(
            "ON_HOVER",
            RawAction {
                kind: "NODE".into(),
                navigation: Some("CHANGE_TO".into()),
                transition: Some(RawTransition {
                    kind: "SMART_ANIMATE".into(),
                    duration: Some(0.15),
                    easing: Some(RawEasing {
                        kind: "LINEAR".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let click = reaction(
            "ON_CLICK",
            RawAction {
                kind: "NODE".into(),
                transition: Some(RawTransition {
                    kind: "DISSOLVE".into(),
                    duration: Some(1.0),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );

        let with_hover = read_reactions(&[hover.clone(), click.clone()]);
        assert_eq!(
            css_state_transition(&with_hover).unwrap().duration_ms,
            150.0,
            "the hover transition should win, not the click one"
        );

        let click_only = read_reactions(&[click]);
        assert!(css_state_transition(&click_only).is_none());
    }
}
