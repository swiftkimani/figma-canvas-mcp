//! A terse, line-per-node view of a scene.
//!
//! Why this is the default rather than the full IR: a model reading a design
//! almost always needs *structure* first — what is here, how it nests, what is a
//! component — and only then the details of one subtree. Sending the whole IR for
//! a 150-node screen costs tens of thousands of tokens, most of which are never
//! used, and on a free tier that is the whole context window.
//!
//! The outline of that same screen is roughly 3% of the size and still carries
//! every node id, so drilling into one branch is one cheap follow-up call.

use std::fmt::Write as _;

use crate::ir::{Direction, Layout, Node, Placement, Sizing};

pub struct Budget {
    /// Stop after this many nodes and say so.
    pub max_nodes: usize,
    /// How many levels to show. 1 means roots only.
    pub max_depth: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_nodes: 300,
            max_depth: 12,
        }
    }
}

/// The key, printed once, so the abbreviations below cost nothing to explain.
pub const LEGEND: &str = "\
id name · TYPE W×H · layout · sizing · paint
  layout:  row|col gap<n> <justify>/<align> p<padding>  ·  abs(<x>,<y>)
  sizing:  w:fill|hug  h:fill|hug   (omitted when fixed)
  paint:   bg:<token-or-colour>  ·  →export for vectors  ·  ⚡ has interactions";

fn num(v: f64) -> String {
    if (v - v.round()).abs() < 0.5 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.0}")
    }
}

fn layout_summary(node: &Node) -> String {
    match &node.layout {
        Layout::Flex {
            direction,
            gap,
            wrap,
            justify,
            align,
            ..
        } => {
            let mut s = String::from(if *direction == Direction::Row {
                "row"
            } else {
                "col"
            });
            if *gap > 0.0 {
                let _ = write!(s, " gap{}", num(*gap));
            }
            // Only mention alignment when it is not the CSS default.
            let j = match justify {
                crate::ir::Justify::Start => "",
                crate::ir::Justify::Center => "center",
                crate::ir::Justify::End => "end",
                crate::ir::Justify::SpaceBetween => "between",
            };
            let a = match align {
                crate::ir::Align::Start => "",
                crate::ir::Align::Center => "center",
                crate::ir::Align::End => "end",
                crate::ir::Align::Baseline => "baseline",
                crate::ir::Align::Stretch => "stretch",
            };
            if !j.is_empty() || !a.is_empty() {
                let _ = write!(
                    s,
                    " {}/{}",
                    if j.is_empty() { "start" } else { j },
                    if a.is_empty() { "start" } else { a }
                );
            }
            if *wrap {
                s.push_str(" wrap");
            }
            if !node.padding.is_zero() {
                match node.padding.uniform() {
                    Some(v) => {
                        let _ = write!(s, " p{}", num(v));
                    }
                    None => {
                        let _ = write!(s, " p{},{}", num(node.padding.top), num(node.padding.left));
                    }
                }
            }
            s
        }
        Layout::Grid { gap } => format!("grid gap{}", num(*gap)),
        Layout::Absolute => String::new(),
    }
}

fn sizing_summary(node: &Node) -> String {
    let (w, h) = match &node.placement {
        Placement::InFlow { width, height, .. } => (width, height),
        Placement::Absolute { .. } => return String::new(),
    };
    let mut parts = Vec::new();
    // Fixed is the uninteresting case; W×H already shows it.
    match w {
        Sizing::Fill => parts.push("w:fill".to_string()),
        Sizing::Hug => parts.push("w:hug".to_string()),
        Sizing::Fixed(_) => {}
    }
    match h {
        Sizing::Fill => parts.push("h:fill".to_string()),
        Sizing::Hug => parts.push("h:hug".to_string()),
        Sizing::Fixed(_) => {}
    }
    parts.join(" ")
}

fn paint_summary(node: &Node) -> String {
    let mut parts = Vec::new();
    if let Some(f) = node.style.fills.first() {
        // A token name is far more useful to a model than a hex value.
        let v = f.token.clone().unwrap_or_else(|| {
            if f.value.is_empty() {
                f.kind.clone()
            } else {
                f.value.clone()
            }
        });
        parts.push(format!("bg:{v}"));
    }
    if node.is_vector() {
        parts.push("→export".into());
    }
    if !node.interactions.is_empty() {
        parts.push(format!("⚡{}", node.interactions.len()));
    }
    parts.join(" ")
}

fn line(node: &Node, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    let _ = write!(
        out,
        "{indent}{} {} · {} {}×{}",
        node.id,
        node.name,
        node.kind,
        num(node.width),
        num(node.height)
    );

    for extra in [
        layout_summary(node),
        sizing_summary(node),
        paint_summary(node),
    ] {
        if !extra.is_empty() {
            let _ = write!(out, " · {extra}");
        }
    }

    // An instance's identity is the most decision-relevant fact about it.
    if let Some(i) = &node.instance {
        let _ = write!(out, " · <{}", i.tag());
        for (k, v) in &i.props {
            let _ = write!(out, " {}={}", k.split('#').next().unwrap_or(k), v);
        }
        out.push('>');
    }

    // Text content is why a text node exists, so it is never elided entirely.
    if let Some(t) = &node.text {
        let c = t.content.replace('\n', " ");
        let shown = if c.chars().count() > 60 {
            let truncated: String = c.chars().take(57).collect();
            format!("{truncated}...")
        } else {
            c
        };
        let _ = write!(out, " · \"{shown}\"");
    }

    out.push('\n');
}

struct Walker<'a> {
    budget: &'a Budget,
    emitted: usize,
    skipped: usize,
}

impl Walker<'_> {
    fn walk(&mut self, node: &Node, depth: usize, out: &mut String) {
        if self.emitted >= self.budget.max_nodes {
            self.skipped += node.count();
            return;
        }
        line(node, depth, out);
        self.emitted += 1;

        if depth + 1 >= self.budget.max_depth {
            let hidden: usize = node.children.iter().map(Node::count).sum();
            if hidden > 0 {
                let _ = writeln!(
                    out,
                    "{}… {hidden} more node(s) below depth {}",
                    "  ".repeat(depth + 1),
                    self.budget.max_depth
                );
                self.skipped += hidden;
            }
            return;
        }
        for c in &node.children {
            self.walk(c, depth + 1, out);
        }
    }
}

/// Render an outline for one or more roots.
pub fn render(roots: &[Node], budget: &Budget) -> String {
    let total: usize = roots.iter().map(Node::count).sum();
    let mut body = String::new();
    let mut w = Walker {
        budget,
        emitted: 0,
        skipped: 0,
    };
    for r in roots {
        w.walk(r, 0, &mut body);
    }

    let mut out = format!("{total} node(s).\n\n{LEGEND}\n\n{body}");
    if w.skipped > 0 {
        let _ = write!(
            out,
            "\n{} node(s) not shown. Raise max_nodes/depth, or call read_scene \
             with detail=\"full\" and the node_ids you care about.\n",
            w.skipped
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::{RawNode, RawPadding, RawRect, RawText};

    fn scene() -> Node {
        let child = RawNode {
            id: "1:2".into(),
            name: "Label".into(),
            kind: "TEXT".into(),
            visible: true,
            absolute_bounding_box: Some(RawRect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 20.0,
            }),
            layout_sizing_horizontal: Some("FILL".into()),
            text: Some(RawText {
                characters: "Hello world".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let root = RawNode {
            id: "1:1".into(),
            name: "Card".into(),
            kind: "FRAME".into(),
            visible: true,
            absolute_bounding_box: Some(RawRect {
                x: 0.0,
                y: 0.0,
                width: 320.0,
                height: 80.0,
            }),
            layout_mode: Some("VERTICAL".into()),
            item_spacing: Some(12.0),
            padding: Some(RawPadding {
                top: 16.0,
                right: 16.0,
                bottom: 16.0,
                left: 16.0,
            }),
            children: vec![child],
            ..Default::default()
        };
        crate::ir::build(&root)
    }

    #[test]
    fn a_node_fits_on_one_line_with_its_id_intact() {
        let out = render(&[scene()], &Budget::default());
        let body: Vec<&str> = out
            .lines()
            .filter(|l| l.contains("1:1") || l.contains("1:2"))
            .collect();
        assert_eq!(body.len(), 2, "one line per node:\n{out}");
        assert!(body[0].contains("1:1 Card · FRAME 320×80"), "{}", body[0]);
        assert!(body[0].contains("col gap12 p16"), "{}", body[0]);
        // The id must survive: it is how a follow-up call drills in.
        assert!(body[1].contains("1:2"), "{}", body[1]);
        assert!(body[1].contains("w:fill"), "{}", body[1]);
        assert!(body[1].contains("\"Hello world\""), "{}", body[1]);
    }

    #[test]
    fn the_outline_is_far_cheaper_than_the_full_ir() {
        let s = scene();
        let full = serde_json::to_string(&s).unwrap();
        let outline = render(&[s], &Budget::default());
        // Excluding the fixed legend, which is paid once per call.
        let body = outline.split(LEGEND).nth(1).unwrap();
        assert!(
            body.len() * 4 < full.len(),
            "outline {} bytes vs full IR {} bytes",
            body.len(),
            full.len()
        );
    }

    #[test]
    fn a_node_budget_truncates_and_says_so() {
        let mut root = scene();
        // 40 siblings, budget of 5.
        let child = root.children[0].clone();
        root.children = std::iter::repeat_n(child, 40).collect();

        let out = render(
            &[root],
            &Budget {
                max_nodes: 5,
                max_depth: 12,
            },
        );
        assert!(out.contains("not shown"), "must admit truncation:\n{out}");
        assert!(
            out.contains("detail=\"full\""),
            "must say how to get more:\n{out}"
        );
        let shown = out.lines().filter(|l| l.contains("1:2 Label")).count();
        assert!(shown <= 5, "emitted {shown} nodes despite a budget of 5");
    }

    #[test]
    fn depth_limit_reports_what_it_hid() {
        let out = render(
            &[scene()],
            &Budget {
                max_nodes: 300,
                max_depth: 1,
            },
        );
        assert!(out.contains("below depth 1"), "{out}");
        assert!(
            !out.contains("Hello world"),
            "depth 1 should not reach the child:\n{out}"
        );
    }
}
