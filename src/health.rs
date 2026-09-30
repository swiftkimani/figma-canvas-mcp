//! What the design itself is costing the output.
//!
//! The same generator produces excellent code from a disciplined file and a pile
//! of absolutely-positioned divs from a dragged-together one. That is not a bug —
//! it is the honest translation of what is there. But silently handing back
//! `position: absolute` and class names like `frame427` leaves the caller with no
//! idea *why* the output is poor or what would fix it.
//!
//! So the scene is assessed and the findings are reported alongside it. Every
//! finding names a specific thing to change in Figma, because "improve your
//! file" is not actionable and "select the frame and press Shift+A" is.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ir::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Materially degrades the generated code.
    Problem,
    /// Worth knowing, but the output is still usable.
    Note,
    /// Something the file did well, and the output is better for it.
    Good,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    pub message: String,
    /// What to change in Figma. None where there is nothing to fix.
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Health {
    pub findings: Vec<Finding>,
}

#[derive(Default)]
struct Counts {
    nodes: usize,
    /// Containers with more than one child — the ones where layout matters.
    containers: usize,
    auto_layout: usize,
    fills: usize,
    token_fills: usize,
    default_names: usize,
    instances: usize,
    vectors: usize,
    /// Containers holding exactly one child and adding nothing themselves.
    redundant_wrappers: usize,
    /// structure key -> how many siblings share it, for runs of 3 or more.
    repeated_shapes: HashMap<u64, usize>,
    absolute_children: usize,
    truncated: bool,
}

/// Names Figma assigns when nobody bothered.
fn is_default_name(name: &str) -> bool {
    const STOCK: [&str; 13] = [
        "frame",
        "group",
        "rectangle",
        "ellipse",
        "vector",
        "line",
        "polygon",
        "star",
        "component",
        "union",
        "subtract",
        "intersect",
        "exclude",
    ];
    let lower = name.trim().to_lowercase();
    if lower == "text" {
        return true;
    }
    for word in STOCK {
        // Bare "Rectangle" is a default, and so is "Frame 427". "Frame for
        // sidebar" is a deliberate choice and must not be flagged.
        if lower == word {
            return true;
        }
        if let Some(rest) = lower.strip_prefix(word) {
            let rest = rest.trim();
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }
    false
}

/// Does this container earn its place in the tree?
fn is_redundant_wrapper(node: &Node) -> bool {
    if node.children.len() != 1 {
        return false;
    }
    let child = &node.children[0];
    node.style == Default::default()
        && node.padding.is_zero()
        && node.interactions.is_empty()
        && matches!(node.layout, crate::ir::Layout::Absolute)
        // Same size as its only child, so it is not constraining anything.
        && (node.width - child.width).abs() < 1.0
        && (node.height - child.height).abs() < 1.0
}

fn walk(node: &Node, c: &mut Counts) {
    c.nodes += 1;
    if node.truncated {
        c.truncated = true;
    }
    if is_default_name(&node.name) {
        c.default_names += 1;
    }
    if node.instance.is_some() {
        c.instances += 1;
        return; // instance internals are the component's business
    }
    if node.is_vector() {
        c.vectors += 1;
    }
    c.fills += node.style.fills.len();
    c.token_fills += node
        .style
        .fills
        .iter()
        .filter(|f| f.css_var.is_some())
        .count();

    if node.children.len() > 1 {
        c.containers += 1;
        if !matches!(node.layout, crate::ir::Layout::Absolute) {
            c.auto_layout += 1;
        }
    }
    if is_redundant_wrapper(node) {
        c.redundant_wrappers += 1;
    }

    // Runs of identical siblings: the signal for both list rendering and for
    // spotting instances that were detached.
    let mut i = 0;
    while i < node.children.len() {
        let key = node.children[i].structure_key();
        let mut j = i + 1;
        while j < node.children.len() && node.children[j].structure_key() == key {
            j += 1;
        }
        if j - i >= 3 {
            *c.repeated_shapes.entry(key).or_insert(0) += j - i;
        }
        i = j;
    }

    for child in &node.children {
        if matches!(child.placement, crate::ir::Placement::Absolute { .. }) {
            c.absolute_children += 1;
        }
        walk(child, c);
    }
}

fn pct(part: usize, whole: usize) -> usize {
    (part * 100).checked_div(whole).unwrap_or(0)
}

pub fn assess(roots: &[Node]) -> Health {
    let mut c = Counts::default();
    for r in roots {
        walk(r, &mut c);
    }
    let mut findings = Vec::new();

    // --- auto layout: the single biggest determinant of output quality ---
    if c.containers > 0 {
        let covered = pct(c.auto_layout, c.containers);
        if covered == 0 {
            findings.push(Finding {
                severity: Severity::Problem,
                message: format!(
                    "None of the {} multi-child containers use auto layout, so every child is \
                     emitted as position:absolute. The result will not reflow for content, \
                     translation or viewport size.",
                    c.containers
                ),
                fix: Some(
                    "Select a frame in Figma and press Shift+A to add auto layout. \
                     Do the outermost frames first; the gain compounds downwards."
                        .into(),
                ),
            });
        } else if covered < 70 {
            findings.push(Finding {
                severity: Severity::Problem,
                message: format!(
                    "{covered}% of containers use auto layout ({} of {}). The rest become \
                     absolutely positioned and will not reflow.",
                    c.auto_layout, c.containers
                ),
                fix: Some(
                    "Add auto layout (Shift+A) to the frames still positioned by hand.".into(),
                ),
            });
        } else {
            findings.push(Finding {
                severity: Severity::Good,
                message: format!(
                    "{covered}% of containers use auto layout, so the output is real flexbox \
                     rather than fixed coordinates."
                ),
                fix: None,
            });
        }
    }

    // --- variables: whether the result can be themed at all ---
    if c.fills > 0 {
        let bound = pct(c.token_fills, c.fills);
        if bound == 0 {
            findings.push(Finding {
                severity: Severity::Problem,
                message: format!(
                    "No fill is bound to a variable, so all {} colours are emitted as literal \
                     hex. Dark mode and theming will need the stylesheet edited by hand.",
                    c.fills
                ),
                fix: Some(
                    "Create colour variables in Figma and bind the fills to them; they come \
                     through as CSS custom properties with their real names."
                        .into(),
                ),
            });
        } else if bound < 60 {
            findings.push(Finding {
                severity: Severity::Note,
                message: format!(
                    "{bound}% of fills are bound to variables ({} of {}); the remainder become \
                     literal values.",
                    c.token_fills, c.fills
                ),
                fix: Some("Bind the remaining fills to variables to keep them themeable.".into()),
            });
        } else {
            findings.push(Finding {
                severity: Severity::Good,
                message: format!(
                    "{bound}% of fills carry variable names, so the output is themeable."
                ),
                fix: None,
            });
        }
    }

    // --- naming: class names come from layer names ---
    if c.nodes > 0 {
        let defaults = pct(c.default_names, c.nodes);
        if defaults > 40 {
            findings.push(Finding {
                severity: Severity::Problem,
                message: format!(
                    "{defaults}% of layers still have default names ({} of {}), e.g. \
                     \"Frame 427\". Class names are derived from layer names, so the stylesheet \
                     will be a list of frame427 and group12.",
                    c.default_names, c.nodes
                ),
                fix: Some(
                    "Rename the layers you care about before generating. This is the cheapest \
                     change with the largest effect on how readable the output is."
                        .into(),
                ),
            });
        } else if defaults > 10 {
            findings.push(Finding {
                severity: Severity::Note,
                message: format!(
                    "{defaults}% of layers have default names; those class names will be unhelpful."
                ),
                fix: None,
            });
        }
    }

    // --- detached instances: the diagnosis only this tool can make ---
    let repeated: usize = c.repeated_shapes.values().sum();
    if repeated >= 3 && c.instances == 0 {
        findings.push(Finding {
            severity: Severity::Problem,
            message: format!(
                "{repeated} nodes repeat {} identical structure(s), but the selection contains \
                 no component instances. These look like detached copies.",
                c.repeated_shapes.len()
            ),
            fix: Some(
                "Turn one into a component and swap the copies for instances. The output then \
                 references your component by name and with its variant props, instead of \
                 duplicating the markup."
                    .into(),
            ),
        });
    } else if repeated >= 3 {
        findings.push(Finding {
            severity: Severity::Good,
            message: format!(
                "{repeated} repeated nodes collapsed into {} list render(s) rather than being \
                 duplicated.",
                c.repeated_shapes.len()
            ),
            fix: None,
        });
    }

    // --- structural noise ---
    if c.redundant_wrappers >= 3 {
        findings.push(Finding {
            severity: Severity::Note,
            message: format!(
                "{} container(s) wrap a single child while adding no size, padding or paint. \
                 Each becomes a div that does nothing.",
                c.redundant_wrappers
            ),
            fix: Some("Ungroup them in Figma (Cmd/Ctrl+Shift+G) to flatten the output.".into()),
        });
    }

    if c.truncated {
        findings.push(Finding {
            severity: Severity::Problem,
            message: "Part of this tree was deeper than the conversion limit and was not read."
                .into(),
            fix: Some("Read the nested frames directly by node_id.".into()),
        });
    }

    if c.vectors > 0 {
        findings.push(Finding {
            severity: Severity::Note,
            message: format!(
                "{} vector node(s) are referenced as assets; the code will not render until \
                 they are exported.",
                c.vectors
            ),
            fix: Some("Run export_assets on this selection.".into()),
        });
    }

    Health { findings }
}

impl Health {
    /// Render for a tool response. Empty when there is nothing worth saying.
    pub fn report(&self) -> String {
        if self.findings.is_empty() {
            return String::new();
        }
        let mut out = String::from("Design health:\n");
        // Problems first: they are what changes the output.
        for wanted in [Severity::Problem, Severity::Note, Severity::Good] {
            for f in self.findings.iter().filter(|f| f.severity == wanted) {
                let mark = match f.severity {
                    Severity::Problem => "!",
                    Severity::Note => "-",
                    Severity::Good => "+",
                };
                out.push_str(&format!("  {mark} {}\n", f.message));
                if let Some(fix) = &f.fix {
                    out.push_str(&format!("    fix: {fix}\n"));
                }
            }
        }
        out
    }

    pub fn problems(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Problem)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::*;

    fn rect(w: f64, h: f64) -> Option<RawRect> {
        Some(RawRect {
            x: 0.0,
            y: 0.0,
            width: w,
            height: h,
        })
    }

    /// `disciplined` controls the whole posture: auto layout, bound variables,
    /// real child names and a component instance, or none of those.
    fn card(id: usize, name: &str, disciplined: bool) -> RawNode {
        let auto = disciplined;
        let token = disciplined;
        RawNode {
            id: format!("c{id}"),
            name: name.into(),
            kind: "FRAME".into(),
            visible: true,
            absolute_bounding_box: rect(200.0, 100.0),
            layout_mode: auto.then(|| "VERTICAL".to_string()),
            item_spacing: auto.then_some(8.0),
            fills: vec![RawPaint {
                kind: "SOLID".into(),
                visible: Some(true),
                color: Some(RawColor {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: None,
                }),
                variable_name: token.then(|| "color/surface".to_string()),
                ..Default::default()
            }],
            children: vec![
                RawNode {
                    id: format!("c{id}a"),
                    // An undisciplined file leaves text layers called "Text".
                    name: if disciplined { "Label" } else { "Text" }.into(),
                    kind: "TEXT".into(),
                    visible: true,
                    text: Some(RawText {
                        characters: "hi".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                RawNode {
                    id: format!("c{id}b"),
                    name: if disciplined { "Value" } else { "Text" }.into(),
                    kind: "TEXT".into(),
                    visible: true,
                    text: Some(RawText {
                        characters: "42".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                // A disciplined file reaches for a component here; a dragged one
                // pastes a group and detaches it.
                if disciplined {
                    RawNode {
                        id: format!("c{id}i"),
                        name: "Badge".into(),
                        kind: "INSTANCE".into(),
                        visible: true,
                        instance: Some(RawInstance {
                            component_name: Some("Badge/positive".into()),
                            component_set_name: Some("Badge".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }
                } else {
                    RawNode {
                        id: format!("c{id}i"),
                        name: "Group 9".into(),
                        kind: "GROUP".into(),
                        visible: true,
                        ..Default::default()
                    }
                },
            ],
            ..Default::default()
        }
    }

    fn page(children: Vec<RawNode>, auto: bool) -> Vec<Node> {
        vec![crate::ir::build(&RawNode {
            id: "root".into(),
            name: "Dashboard".into(),
            kind: "FRAME".into(),
            visible: true,
            absolute_bounding_box: rect(800.0, 600.0),
            layout_mode: auto.then(|| "VERTICAL".to_string()),
            children,
            ..Default::default()
        })]
    }

    fn messages(h: &Health, sev: Severity) -> String {
        h.findings
            .iter()
            .filter(|f| f.severity == sev)
            .map(|f| f.message.clone())
            .collect::<Vec<_>>()
            .join(" | ")
    }

    #[test]
    fn default_figma_names_are_recognised_but_real_ones_are_not() {
        for name in [
            "Frame 427",
            "Group 12",
            "Rectangle",
            "Text",
            "Ellipse 3",
            "vector 9",
        ] {
            assert!(is_default_name(name), "{name} should count as a default");
        }
        for name in [
            "Metric card",
            "Sidebar",
            "Frame for sidebar",
            "Nav item",
            "Textarea",
        ] {
            assert!(!is_default_name(name), "{name} is a deliberate name");
        }
    }

    #[test]
    fn an_empty_selection_produces_no_noise() {
        let h = assess(&[]);
        assert!(h.findings.is_empty());
        assert_eq!(h.report(), "");
    }

    #[test]
    fn a_disciplined_file_reports_no_problems() {
        let cards = (0..4).map(|i| card(i, "Metric card", true)).collect();
        let h = assess(&page(cards, true));
        assert_eq!(
            h.problems(),
            0,
            "a well-built file should not be criticised:\n{}",
            h.report()
        );
        let good = messages(&h, Severity::Good);
        assert!(good.contains("auto layout"), "{good}");
        assert!(good.contains("variable names"), "{good}");
        assert!(good.contains("collapsed"), "{good}");
    }

    #[test]
    fn a_dragged_together_file_is_diagnosed_with_fixes() {
        let cards = (0..6)
            .map(|i| card(i, &format!("Frame {}", 427 + i), false))
            .collect();
        let h = assess(&page(cards, false));
        let problems = messages(&h, Severity::Problem);

        assert!(
            problems.contains("auto layout"),
            "missing layout finding:\n{problems}"
        );
        assert!(problems.contains("position:absolute"), "{problems}");
        assert!(
            problems.contains("literal hex"),
            "missing token finding:\n{problems}"
        );
        assert!(
            problems.contains("default names"),
            "missing naming finding:\n{problems}"
        );

        // Every problem must come with something to actually do about it.
        for f in h
            .findings
            .iter()
            .filter(|f| f.severity == Severity::Problem)
        {
            assert!(
                f.fix.is_some(),
                "a problem with no fix is just criticism: {}",
                f.message
            );
        }
    }

    #[test]
    fn repeated_structures_with_no_components_are_called_out_as_detached() {
        // Six identical cards and not one instance between them.
        // Disciplined in every respect except that nobody made a component.
        let cards = (0..6)
            .map(|i| {
                let mut c = card(i, "Card", true);
                c.children.pop(); // remove the instance
                c
            })
            .collect();
        let h = assess(&page(cards, true));
        let problems = messages(&h, Severity::Problem);
        assert!(
            problems.contains("detached"),
            "the detached-instance diagnosis is the one only this tool can make:\n{}",
            h.report()
        );
    }

    #[test]
    fn repetition_with_real_instances_is_praised_not_flagged() {
        // These cards already carry a real instance.
        let cards: Vec<RawNode> = (0..6).map(|i| card(i, "Card", true)).collect();
        let h = assess(&page(cards, true));
        assert!(
            !messages(&h, Severity::Problem).contains("detached"),
            "a file using components should not be told it detached them:\n{}",
            h.report()
        );
    }

    #[test]
    fn pointless_wrapper_groups_are_noticed() {
        // Groups that wrap one child and contribute nothing.
        let wrapped: Vec<RawNode> = (0..4)
            .map(|i| RawNode {
                id: format!("w{i}"),
                name: format!("Group {}", 12 + i),
                kind: "GROUP".into(),
                visible: true,
                absolute_bounding_box: rect(200.0, 100.0),
                children: vec![card(i, "Card", true)],
                ..Default::default()
            })
            .collect();
        let h = assess(&page(wrapped, true));
        assert!(
            messages(&h, Severity::Note).contains("wrap a single child"),
            "{}",
            h.report()
        );
    }

    #[test]
    fn problems_are_reported_before_praise() {
        let h = Health {
            findings: vec![
                Finding {
                    severity: Severity::Good,
                    message: "good thing".into(),
                    fix: None,
                },
                Finding {
                    severity: Severity::Problem,
                    message: "bad thing".into(),
                    fix: Some("do this".into()),
                },
            ],
        };
        let r = h.report();
        assert!(
            r.find("bad thing") < r.find("good thing"),
            "problems must lead:\n{r}"
        );
        assert!(r.contains("fix: do this"));
        assert_eq!(h.problems(), 1);
    }
}
