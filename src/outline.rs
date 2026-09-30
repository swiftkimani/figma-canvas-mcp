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
  layout:  row|col gap<n> <justify>/<align> p<padding>  ·  abs
  sizing:  w:fill|hug  h:fill|hug   (omitted when fixed)
  paint:   bg:<token-or-colour>  ·  →export for vectors  ·  ⚡ has interactions
  ×N       N consecutive siblings share this exact shape; only content differs";

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
    /// Node ids whose subtrees were not expanded, so a follow-up call can resume
    /// from exactly there. Truncation without this is silent data loss.
    unexpanded: Vec<String>,
}

impl Walker<'_> {
    fn walk(&mut self, node: &Node, depth: usize, out: &mut String) {
        if self.emitted >= self.budget.max_nodes {
            self.skipped += node.count();
            self.unexpanded.push(node.id.clone());
            return;
        }
        line(node, depth, out);
        self.emitted += 1;

        if depth + 1 >= self.budget.max_depth {
            let hidden: usize = node.children.iter().map(Node::count).sum();
            if hidden > 0 {
                let _ = writeln!(
                    out,
                    "{}… {hidden} node(s) below depth {} — read_scene node_ids=[\"{}\"] to continue",
                    "  ".repeat(depth + 1),
                    self.budget.max_depth,
                    node.id
                );
                self.skipped += hidden;
                self.unexpanded.push(node.id.clone());
            }
            return;
        }
        self.walk_children(&node.children, depth + 1, out);
    }

    /// Walk siblings, collapsing consecutive runs that share a structure key.
    ///
    /// This is where most of the saving is, because real designs are repetitive:
    /// lists, grids, and card decks are one shape repeated. Collapsing is also
    /// *more* informative than repetition — "×50" states a fact about the design
    /// that fifty near-identical lines leave the reader to infer.
    fn walk_children(&mut self, children: &[Node], depth: usize, out: &mut String) {
        let mut i = 0;
        while i < children.len() {
            let key = children[i].structure_key();
            let mut j = i + 1;
            while j < children.len() && children[j].structure_key() == key {
                j += 1;
            }
            let run = &children[i..j];

            if run.len() >= MIN_RUN {
                self.emit_run(run, depth, out);
            } else {
                for c in run {
                    self.walk(c, depth, out);
                }
            }
            i = j;
        }
    }

    fn emit_run(&mut self, run: &[Node], depth: usize, out: &mut String) {
        if self.emitted >= self.budget.max_nodes {
            self.skipped += run.iter().map(Node::count).sum::<usize>();
            self.unexpanded.push(run[0].id.clone());
            return;
        }

        let first = &run[0];
        // The representative carries the shape; walking it once is enough.
        line(first, depth, out);
        self.emitted += 1;
        if depth + 1 < self.budget.max_depth {
            self.walk_children(&first.children, depth + 1, out);
        }

        let indent = "  ".repeat(depth + 1);
        let _ = write!(
            out,
            "{indent}↳ ×{} siblings share this shape: {} … {}",
            run.len(),
            run[0].id,
            run[run.len() - 1].id
        );

        // The varying text is the data half of the repeat — it is what a .map()
        // would iterate, so it must not be lost to the collapse.
        let mut varying: Vec<String> = Vec::new();
        for node in run {
            let t = node.text_content().join(" / ");
            if !t.is_empty() {
                varying.push(t);
            }
        }
        varying.dedup();
        if !varying.is_empty() {
            let shown = varying.len().min(MAX_RUN_VALUES);
            let quoted: Vec<String> = varying[..shown]
                .iter()
                .map(|v| format!("\"{}\"", elide(v, 40)))
                .collect();
            let _ = write!(out, "; content: {}", quoted.join(", "));
            if varying.len() > shown {
                let _ = write!(out, ", +{} more", varying.len() - shown);
            }
        }
        out.push('\n');

        // Siblings after the first are described, not skipped: nothing is lost.
        self.emitted += run.len() - 1;
    }
}

/// Below this, repetition is not worth collapsing.
const MIN_RUN: usize = 3;
/// Cap on distinct content values listed for one collapsed run.
const MAX_RUN_VALUES: usize = 60;

fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max - 1).collect();
    format!("{head}…")
}

/// Render an outline for one or more roots.
pub fn render(roots: &[Node], budget: &Budget) -> String {
    let total: usize = roots.iter().map(Node::count).sum();
    let mut body = String::new();
    let mut w = Walker {
        budget,
        emitted: 0,
        skipped: 0,
        unexpanded: Vec::new(),
    };
    // Roots are siblings too, so a page of similar frames collapses as well.
    w.walk_children(roots, 0, &mut body);

    let mut out = format!("{total} node(s).\n\n{LEGEND}\n\n{body}");
    if w.skipped > 0 {
        // Name the exact resume points. A count alone leaves the caller guessing
        // what it did not see, which is how silent truncation becomes a wrong
        // translation.
        let ids: Vec<String> = w
            .unexpanded
            .iter()
            .take(20)
            .map(|i| format!("\"{i}\""))
            .collect();
        let _ = write!(
            out,
            "\n{} node(s) not expanded. Nothing is lost — continue with:\n               read_scene node_ids=[{}]{}\n",
            w.skipped,
            ids.join(", "),
            if w.unexpanded.len() > 20 {
                format!(" (+{} more resume points)", w.unexpanded.len() - 20)
            } else {
                String::new()
            }
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
    fn repeated_siblings_collapse_to_one_shape_plus_their_content() {
        let mut root = scene();
        let child = root.children[0].clone();
        // 40 rows that differ only in their text, as a real list does.
        root.children = (0..40)
            .map(|i| {
                let mut c = child.clone();
                c.id = format!("1:{}", 100 + i);
                if let Some(t) = &mut c.text {
                    t.content = format!("Row {i}");
                }
                c
            })
            .collect();

        let out = render(&[root], &Budget::default());

        // One representative line, not forty.
        let label_lines = out.lines().filter(|l| l.contains("· TEXT ")).count();
        assert_eq!(label_lines, 1, "the shape should appear once:\n{out}");

        // But the run, its id range and all of its content survive.
        assert!(out.contains("×40 siblings share this shape"), "{out}");
        assert!(
            out.contains("1:100 … 1:139"),
            "id range must be stated:\n{out}"
        );
        assert!(out.contains("\"Row 0\""), "{out}");
        assert!(
            out.contains("\"Row 39\""),
            "no content may be dropped:\n{out}"
        );
        assert!(
            !out.contains("not expanded"),
            "collapsing is not truncation:\n{out}"
        );
    }

    #[test]
    fn distinct_siblings_are_not_collapsed() {
        let mut root = scene();
        let child = root.children[0].clone();
        root.children = (0..4)
            .map(|i| {
                let mut c = child.clone();
                c.id = format!("1:{}", 200 + i);
                // Different sizing makes each one a different shape.
                c.placement = Placement::InFlow {
                    width: Sizing::Fixed(40.0 * (i + 1) as f64),
                    height: Sizing::Hug,
                    align_self: None,
                };
                c
            })
            .collect();

        let out = render(&[root], &Budget::default());
        assert!(
            !out.contains("share this shape"),
            "distinct shapes must not merge:\n{out}"
        );
        for i in 200..204 {
            assert!(
                out.contains(&format!("1:{i}")),
                "node 1:{i} missing:\n{out}"
            );
        }
    }

    #[test]
    fn a_node_budget_names_exact_resume_points() {
        let mut root = scene();
        let child = root.children[0].clone();
        // Structurally distinct children, so collapsing cannot absorb them.
        root.children = (0..30)
            .map(|i| {
                let mut c = child.clone();
                c.id = format!("1:{}", 300 + i);
                c.padding = crate::ir::Edges {
                    top: i as f64,
                    right: 0.0,
                    bottom: 0.0,
                    left: 0.0,
                };
                c
            })
            .collect();

        let out = render(
            &[root],
            &Budget {
                max_nodes: 5,
                max_depth: 12,
            },
        );
        assert!(
            out.contains("not expanded"),
            "must admit what it did not walk:\n{out}"
        );
        assert!(
            out.contains("read_scene node_ids=["),
            "must name resume points, not just a count:\n{out}"
        );
        assert!(out.contains("Nothing is lost"), "{out}");
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
