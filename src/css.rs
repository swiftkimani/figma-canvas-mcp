//! IR -> CSS declarations.
//!
//! Two sources of truth are available and they are not equivalent:
//!
//! * `getCSSAsync()` — Figma's own per-node CSS. Authoritative for paint, type and
//!   effects, but it describes a node in isolation and leans on fixed sizes.
//! * our IR — knows the *relationship* between a node and its parent, so it can
//!   emit `flex: 1`, `align-self: stretch` and `fit-content` where Figma would
//!   have written a hard pixel width.
//!
//! So we take layout from the IR and let Figma's CSS fill in the rest. That split
//! is deliberate: it is what stops generated code degenerating into fixed-size
//! absolutely-positioned boxes.

use crate::ir::{Direction, Edges, Layout, Node, Placement, Sizing};

/// Ordered declarations, so output diffs stay readable.
pub type Decls = Vec<(String, String)>;

fn px(v: f64) -> String {
    if (v - v.round()).abs() < 0.01 {
        format!("{}px", v.round() as i64)
    } else {
        format!("{v:.2}px")
    }
}

fn edges(e: &Edges) -> String {
    match e.uniform() {
        Some(v) => px(v),
        None => {
            if e.top == e.bottom && e.left == e.right {
                format!("{} {}", px(e.top), px(e.left))
            } else {
                format!(
                    "{} {} {} {}",
                    px(e.top),
                    px(e.right),
                    px(e.bottom),
                    px(e.left)
                )
            }
        }
    }
}

/// Which CSS properties we are willing to accept from `getCSSAsync()`.
/// Layout belongs to us; everything visual belongs to Figma.
fn figma_css_is_layout(prop: &str) -> bool {
    matches!(
        prop,
        "display"
            | "flex-direction"
            | "align-items"
            | "justify-content"
            | "gap"
            | "row-gap"
            | "column-gap"
            | "flex-wrap"
            | "flex"
            | "flex-grow"
            | "flex-shrink"
            | "flex-basis"
            | "align-self"
            | "width"
            | "height"
            | "min-width"
            | "min-height"
            | "max-width"
            | "max-height"
            | "padding"
            | "padding-top"
            | "padding-right"
            | "padding-bottom"
            | "padding-left"
            | "position"
            | "top"
            | "right"
            | "bottom"
            | "left"
    )
}

/// Build the declaration list for one node.
///
/// `parent` is the parent's layout, which decides whether FILL means `flex: 1` or
/// `align-self: stretch`. Pass `None` for the root.
pub fn declarations(node: &Node, parent: Option<&Layout>) -> Decls {
    let mut d: Decls = Vec::new();

    // --- this node as a container ---
    match &node.layout {
        Layout::Flex {
            direction,
            gap,
            cross_gap,
            wrap,
            justify,
            align,
        } => {
            d.push(("display".into(), "flex".into()));
            if *direction == Direction::Column {
                d.push(("flex-direction".into(), "column".into()));
            }
            if *wrap {
                d.push(("flex-wrap".into(), "wrap".into()));
            }
            match (cross_gap, wrap) {
                (Some(cg), true) if (cg - gap).abs() > f64::EPSILON => {
                    d.push(("row-gap".into(), px(*cg)));
                    d.push(("column-gap".into(), px(*gap)));
                }
                _ if *gap > 0.0 => d.push(("gap".into(), px(*gap))),
                _ => {}
            }
            // Figma's MIN is flex-start, which is already the CSS default.
            if justify.css() != "flex-start" {
                d.push(("justify-content".into(), justify.css().into()));
            }
            if align.css() != "flex-start" {
                d.push(("align-items".into(), align.css().into()));
            }
        }
        Layout::Grid { gap } => {
            d.push(("display".into(), "grid".into()));
            if *gap > 0.0 {
                d.push(("gap".into(), px(*gap)));
            }
        }
        Layout::Absolute => {
            // Only establish a containing block if a child actually needs one.
            let needs = node
                .children
                .iter()
                .any(|c| matches!(c.placement, Placement::Absolute { .. }));
            if needs {
                d.push(("position".into(), "relative".into()));
            }
        }
    }

    if !node.padding.is_zero() {
        d.push(("padding".into(), edges(&node.padding)));
    }

    // --- this node inside its parent ---
    let parent_dir = match parent {
        Some(Layout::Flex { direction, .. }) => Some(*direction),
        _ => None,
    };

    match &node.placement {
        Placement::InFlow {
            width,
            height,
            align_self,
        } => {
            let (main, cross) = match parent_dir {
                Some(Direction::Row) => (width, height),
                Some(Direction::Column) => (height, width),
                None => (width, height),
            };

            // Main axis.
            match main {
                Sizing::Fill => d.push(("flex".into(), "1 1 0%".into())),
                Sizing::Hug => {}
                Sizing::Fixed(v) => {
                    let prop = if parent_dir == Some(Direction::Column) {
                        "height"
                    } else {
                        "width"
                    };
                    d.push((prop.into(), px(*v)));
                    // Stop flex from shrinking a size the designer fixed.
                    d.push(("flex-shrink".into(), "0".into()));
                }
            }

            // Cross axis.
            match cross {
                Sizing::Fill => d.push(("align-self".into(), "stretch".into())),
                Sizing::Hug => {}
                Sizing::Fixed(v) => {
                    let prop = if parent_dir == Some(Direction::Column) {
                        "width"
                    } else {
                        "height"
                    };
                    d.push((prop.into(), px(*v)));
                }
            }

            if let Some(a) = align_self {
                if !d.iter().any(|(k, _)| k == "align-self") {
                    d.push(("align-self".into(), a.css().into()));
                }
            }
        }
        Placement::Absolute {
            x,
            y,
            width,
            height,
            anchor_h,
            anchor_v,
        } => {
            d.push(("position".into(), "absolute".into()));
            use crate::ir::Anchor;
            match anchor_h {
                Anchor::Max => d.push(("right".into(), px(0.0f64.max(*x)))),
                Anchor::Center => {
                    d.push(("left".into(), "50%".into()));
                    d.push(("transform".into(), "translateX(-50%)".into()));
                }
                Anchor::Stretch => {
                    d.push(("left".into(), px(*x)));
                    d.push(("right".into(), px(*x)));
                }
                _ => d.push(("left".into(), px(*x))),
            }
            match anchor_v {
                Anchor::Max => d.push(("bottom".into(), px(0.0f64.max(*y)))),
                Anchor::Center => d.push(("top".into(), "50%".into())),
                Anchor::Stretch => {
                    d.push(("top".into(), px(*y)));
                    d.push(("bottom".into(), px(*y)));
                }
                _ => d.push(("top".into(), px(*y))),
            }
            if let Sizing::Fixed(w) = width {
                d.push(("width".into(), px(*w)));
            }
            if let Sizing::Fixed(h) = height {
                d.push(("height".into(), px(*h)));
            }
        }
    }

    // --- appearance: prefer Figma's own CSS, skipping anything layout-related ---
    let mut styled_from_figma = false;
    if let Some(fcss) = &node.figma_css {
        for (k, v) in fcss {
            if figma_css_is_layout(k) {
                continue;
            }
            d.push((k.clone(), v.clone()));
            styled_from_figma = true;
        }
    }

    if !styled_from_figma {
        appearance(node, &mut d);
    }

    // Token-bound fills always win: a var() reference survives a theme change,
    // a literal hex from getCSSAsync() does not.
    if let Some(fill) = node
        .style
        .fills
        .iter()
        .find(|f| f.css_var.is_some() && f.kind == "SOLID")
    {
        let prop = if node.text.is_some() {
            "color"
        } else {
            "background"
        };
        d.retain(|(k, _)| k != prop && k != "background-color");
        d.push((prop.into(), fill.css_value()));
    }

    dedupe_last_wins(d)
}

/// Our own appearance mapping, used when `getCSSAsync()` was not requested.
fn appearance(node: &Node, d: &mut Decls) {
    let s = &node.style;

    if let Some(first) = s.fills.first() {
        if first.kind == "IMAGE" {
            d.push(("background-size".into(), "cover".into()));
            d.push(("background-position".into(), "center".into()));
        } else if !first.value.is_empty() || first.css_var.is_some() {
            let prop = if node.text.is_some() {
                "color"
            } else {
                "background"
            };
            d.push((prop.into(), first.css_value()));
        }
    }

    if let (Some(w), Some(stroke)) = (s.stroke_width, s.strokes.first()) {
        d.push(("border".into(), format!("{} solid {}", px(w), stroke.css_value())));
    }

    if !s.radius.is_zero() {
        match s.radius.uniform() {
            Some(r) => d.push(("border-radius".into(), px(r))),
            None => d.push((
                "border-radius".into(),
                format!(
                    "{} {} {} {}",
                    px(s.radius.tl),
                    px(s.radius.tr),
                    px(s.radius.br),
                    px(s.radius.bl)
                ),
            )),
        }
    }

    if !s.shadows.is_empty() {
        let shadows: Vec<String> = s
            .shadows
            .iter()
            .map(|sh| {
                let inset = if sh.inset { "inset " } else { "" };
                format!(
                    "{inset}{} {} {} {} {}",
                    px(sh.x),
                    px(sh.y),
                    px(sh.blur),
                    px(sh.spread),
                    sh.color
                )
            })
            .collect();
        d.push(("box-shadow".into(), shadows.join(", ")));
    }

    if let Some(b) = s.blur {
        d.push(("filter".into(), format!("blur({})", px(b))));
    }
    if let Some(o) = s.opacity {
        d.push(("opacity".into(), format!("{o:.3}")));
    }
    if let Some(r) = s.rotation {
        d.push(("transform".into(), format!("rotate({:.2}deg)", -r)));
    }
    if s.clip {
        d.push(("overflow".into(), "hidden".into()));
    }

    if let Some(t) = &node.text {
        if let Some(f) = &t.font_family {
            d.push(("font-family".into(), format!("\"{f}\"")));
        }
        if let Some(v) = t.font_size {
            d.push(("font-size".into(), px(v)));
        }
        if let Some(v) = t.font_weight {
            d.push(("font-weight".into(), format!("{}", v.round() as i64)));
        }
        if let Some(v) = &t.line_height {
            d.push(("line-height".into(), v.clone()));
        }
        if let Some(v) = &t.letter_spacing {
            d.push(("letter-spacing".into(), v.clone()));
        }
        if let Some(v) = &t.align {
            if v != "left" {
                d.push(("text-align".into(), v.clone()));
            }
        }
        if let Some(v) = &t.transform {
            d.push(("text-transform".into(), v.clone()));
        }
        if let Some(v) = &t.decoration {
            d.push(("text-decoration".into(), v.clone()));
        }
    }
}

/// Later declarations override earlier ones, but keep first-seen ordering.
fn dedupe_last_wins(d: Decls) -> Decls {
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (k, v) in d {
        if !map.contains_key(&k) {
            order.push(k.clone());
        }
        map.insert(k, v);
    }
    order
        .into_iter()
        .filter_map(|k| map.remove(&k).map(|v| (k, v)))
        .collect()
}

/// Keep only the declarations that describe how a node sits in its parent.
///
/// Used for component instances: the component owns its own paint, radius and
/// typography, but *placement* is the caller's decision, so a resized or
/// stretched instance still needs these.
pub fn placement_only(decls: Decls) -> Decls {
    decls
        .into_iter()
        .filter(|(k, _)| {
            matches!(
                k.as_str(),
                "flex"
                    | "flex-grow"
                    | "flex-shrink"
                    | "flex-basis"
                    | "align-self"
                    | "justify-self"
                    | "width"
                    | "height"
                    | "min-width"
                    | "min-height"
                    | "max-width"
                    | "max-height"
                    | "position"
                    | "top"
                    | "right"
                    | "bottom"
                    | "left"
                    | "transform"
            )
        })
        .collect()
}

/// Strip absolute positioning from a component's outermost element.
///
/// The root of a generated component is a boundary: where it sits is the
/// consuming page's business, not ours. Leaving Figma's canvas coordinates on it
/// produces a component that jumps to the top-left of its container.
pub fn strip_root_position(decls: Decls) -> Decls {
    let was_absolute = decls
        .iter()
        .any(|(k, v)| k == "position" && v == "absolute");
    decls
        .into_iter()
        .filter(|(k, _)| {
            !(was_absolute
                && matches!(
                    k.as_str(),
                    "position" | "top" | "right" | "bottom" | "left" | "transform"
                ))
        })
        .collect()
}

/// Render declarations as a CSS rule body.
pub fn to_block(decls: &Decls, indent: &str) -> String {
    decls
        .iter()
        .map(|(k, v)| format!("{indent}{k}: {v};"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Align, Justify, Style};

    fn node(kind: &str, layout: Layout, placement: Placement) -> Node {
        Node {
            id: "1:1".into(),
            name: "n".into(),
            kind: kind.into(),
            visible: true,
            width: 100.0,
            height: 50.0,
            layout,
            placement,
            padding: Edges::default(),
            style: Style::default(),
            text: None,
            instance: None,
            figma_css: None,
            tokens: Default::default(),
            exportable: false,
            children: vec![],
        }
    }

    fn get<'a>(d: &'a Decls, k: &str) -> Option<&'a str> {
        d.iter().find(|(p, _)| p == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn row_flex_omits_css_defaults() {
        let n = node(
            "FRAME",
            Layout::Flex {
                direction: Direction::Row,
                gap: 8.0,
                cross_gap: None,
                wrap: false,
                justify: Justify::Start,
                align: Align::Start,
            },
            Placement::InFlow {
                width: Sizing::Hug,
                height: Sizing::Hug,
                align_self: None,
            },
        );
        let d = declarations(&n, None);
        assert_eq!(get(&d, "display"), Some("flex"));
        assert_eq!(get(&d, "gap"), Some("8px"));
        // flex-start and row are CSS defaults; emitting them is noise.
        assert_eq!(get(&d, "justify-content"), None);
        assert_eq!(get(&d, "align-items"), None);
        assert_eq!(get(&d, "flex-direction"), None);
    }

    #[test]
    fn fill_on_main_axis_becomes_flex_not_width() {
        let parent = Layout::Flex {
            direction: Direction::Row,
            gap: 0.0,
            cross_gap: None,
            wrap: false,
            justify: Justify::Start,
            align: Align::Start,
        };
        let n = node(
            "FRAME",
            Layout::Absolute,
            Placement::InFlow {
                width: Sizing::Fill,
                height: Sizing::Hug,
                align_self: None,
            },
        );
        let d = declarations(&n, Some(&parent));
        assert_eq!(get(&d, "flex"), Some("1 1 0%"));
        assert_eq!(get(&d, "width"), None, "FILL must not become a fixed width");
    }

    #[test]
    fn fill_on_cross_axis_becomes_align_self_stretch() {
        let parent = Layout::Flex {
            direction: Direction::Column,
            gap: 0.0,
            cross_gap: None,
            wrap: false,
            justify: Justify::Start,
            align: Align::Start,
        };
        let n = node(
            "FRAME",
            Layout::Absolute,
            Placement::InFlow {
                width: Sizing::Fill,
                height: Sizing::Hug,
                align_self: None,
            },
        );
        let d = declarations(&n, Some(&parent));
        assert_eq!(get(&d, "align-self"), Some("stretch"));
        assert_eq!(get(&d, "flex"), None);
    }

    #[test]
    fn fixed_child_of_flex_gets_shrink_zero() {
        let parent = Layout::Flex {
            direction: Direction::Row,
            gap: 0.0,
            cross_gap: None,
            wrap: false,
            justify: Justify::Start,
            align: Align::Start,
        };
        let n = node(
            "FRAME",
            Layout::Absolute,
            Placement::InFlow {
                width: Sizing::Fixed(40.0),
                height: Sizing::Hug,
                align_self: None,
            },
        );
        let d = declarations(&n, Some(&parent));
        assert_eq!(get(&d, "width"), Some("40px"));
        assert_eq!(get(&d, "flex-shrink"), Some("0"));
    }

    #[test]
    fn figma_css_cannot_override_our_layout() {
        let mut n = node(
            "FRAME",
            Layout::Flex {
                direction: Direction::Row,
                gap: 4.0,
                cross_gap: None,
                wrap: false,
                justify: Justify::Start,
                align: Align::Start,
            },
            Placement::InFlow {
                width: Sizing::Fill,
                height: Sizing::Hug,
                align_self: None,
            },
        );
        let mut fcss = std::collections::BTreeMap::new();
        // Figma would hard-code this width; our FILL must survive.
        fcss.insert("width".to_string(), "327px".to_string());
        fcss.insert("border-radius".to_string(), "12px".to_string());
        n.figma_css = Some(fcss);

        let parent = Layout::Flex {
            direction: Direction::Row,
            gap: 0.0,
            cross_gap: None,
            wrap: false,
            justify: Justify::Start,
            align: Align::Start,
        };
        let d = declarations(&n, Some(&parent));
        assert_eq!(get(&d, "width"), None, "layout must come from the IR");
        assert_eq!(get(&d, "flex"), Some("1 1 0%"));
        assert_eq!(get(&d, "border-radius"), Some("12px"), "visuals come from Figma");
    }

    #[test]
    fn absolute_child_pins_by_constraint() {
        let n = node(
            "FRAME",
            Layout::Absolute,
            Placement::Absolute {
                x: 16.0,
                y: 24.0,
                width: Sizing::Fixed(32.0),
                height: Sizing::Fixed(32.0),
                anchor_h: crate::ir::Anchor::Max,
                anchor_v: crate::ir::Anchor::Min,
            },
        );
        let d = declarations(&n, None);
        assert_eq!(get(&d, "position"), Some("absolute"));
        assert_eq!(get(&d, "right"), Some("16px"));
        assert_eq!(get(&d, "top"), Some("24px"));
        assert_eq!(get(&d, "left"), None);
    }
}
