//! How does output degrade between a disciplined file and a real one?
//!
//! Two designs of comparable size describing the same dashboard. One was built
//! by someone who uses auto-layout, components and variables. The other is what
//! files actually look like: absolute positioning, "Frame 427", detached
//! instances, hardcoded hex, and pointless nested groups.

use figma_canvas_mcp::codegen::{self, StyleMode};
use figma_canvas_mcp::outline::{self, Budget};
use figma_canvas_mcp::raw::*;
use figma_canvas_mcp::stack::Stack;
use figma_canvas_mcp::{ir, ir::Node};

fn rect(w: f64, h: f64) -> Option<RawRect> {
    Some(RawRect {
        x: 0.0,
        y: 0.0,
        width: w,
        height: h,
    })
}

fn token_fill(name: &str, r: f64, g: f64, b: f64) -> RawPaint {
    RawPaint {
        kind: "SOLID".into(),
        visible: Some(true),
        opacity: Some(1.0),
        color: Some(RawColor { r, g, b, a: None }),
        variable_name: Some(name.into()),
        variable_id: Some(format!("V:{name}")),
        ..Default::default()
    }
}

fn literal_fill(r: f64, g: f64, b: f64) -> RawPaint {
    RawPaint {
        kind: "SOLID".into(),
        visible: Some(true),
        opacity: Some(1.0),
        color: Some(RawColor { r, g, b, a: None }),
        ..Default::default()
    }
}

fn text(id: &str, name: &str, content: &str, size: f64) -> RawNode {
    RawNode {
        id: id.into(),
        name: name.into(),
        kind: "TEXT".into(),
        visible: true,
        absolute_bounding_box: rect(160.0, size * 1.4),
        text: Some(RawText {
            characters: content.into(),
            font_family: Some("Inter".into()),
            font_size: Some(size),
            font_weight: Some(if size > 20.0 { 700.0 } else { 400.0 }),
            line_height: Some(format!("{}px", (size * 1.4).round())),
            ..Default::default()
        }),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// The disciplined file
// ---------------------------------------------------------------------------

fn organised() -> RawNode {
    let card = |i: usize| RawNode {
        id: format!("2:{i}"),
        name: "Metric card".into(),
        kind: "FRAME".into(),
        visible: true,
        absolute_bounding_box: rect(280.0, 140.0),
        layout_mode: Some("VERTICAL".into()),
        item_spacing: Some(8.0),
        counter_axis_align_items: Some("MIN".into()),
        padding: Some(RawPadding {
            top: 20.0,
            right: 20.0,
            bottom: 20.0,
            left: 20.0,
        }),
        layout_sizing_horizontal: Some("FILL".into()),
        layout_sizing_vertical: Some("HUG".into()),
        corner_radius: Some(RawRadius {
            top_left: 12.0,
            top_right: 12.0,
            bottom_right: 12.0,
            bottom_left: 12.0,
        }),
        fills: vec![token_fill("color/surface/raised", 1.0, 1.0, 1.0)],
        bound_variables: [("fills".to_string(), "color/surface/raised".to_string())]
            .into_iter()
            .collect(),
        children: vec![
            text(&format!("2:{i}a"), "Label", &format!("Metric {i}"), 13.0),
            text(
                &format!("2:{i}b"),
                "Value",
                &format!("{}", (i + 1) * 1247),
                28.0,
            ),
            RawNode {
                id: format!("2:{i}c"),
                name: "Trend badge".into(),
                kind: "INSTANCE".into(),
                visible: true,
                absolute_bounding_box: rect(64.0, 24.0),
                instance: Some(RawInstance {
                    component_id: Some("10:1".into()),
                    component_name: Some("Badge/positive".into()),
                    component_set_name: Some("Badge".into()),
                    properties: [
                        ("Tone".to_string(), "positive".to_string()),
                        ("Size".to_string(), "sm".to_string()),
                    ]
                    .into_iter()
                    .collect(),
                    is_remote: true,
                }),
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let nav_item = |i: usize| RawNode {
        id: format!("3:{i}"),
        name: "Nav item".into(),
        kind: "FRAME".into(),
        visible: true,
        absolute_bounding_box: rect(200.0, 40.0),
        layout_mode: Some("HORIZONTAL".into()),
        item_spacing: Some(12.0),
        counter_axis_align_items: Some("CENTER".into()),
        padding: Some(RawPadding {
            top: 8.0,
            right: 12.0,
            bottom: 8.0,
            left: 12.0,
        }),
        layout_sizing_horizontal: Some("FILL".into()),
        layout_sizing_vertical: Some("HUG".into()),
        children: vec![
            RawNode {
                id: format!("3:{i}i"),
                name: "Icon".into(),
                kind: "VECTOR".into(),
                visible: true,
                absolute_bounding_box: rect(16.0, 16.0),
                ..Default::default()
            },
            text(&format!("3:{i}t"), "Label", &format!("Section {i}"), 14.0),
        ],
        ..Default::default()
    };

    RawNode {
        id: "1:1".into(),
        name: "Dashboard".into(),
        kind: "FRAME".into(),
        visible: true,
        absolute_bounding_box: rect(1440.0, 900.0),
        layout_mode: Some("HORIZONTAL".into()),
        item_spacing: Some(0.0),
        fills: vec![token_fill("color/surface/base", 0.97, 0.97, 0.98)],
        children: vec![
            RawNode {
                id: "1:2".into(),
                name: "Sidebar".into(),
                kind: "FRAME".into(),
                visible: true,
                absolute_bounding_box: rect(240.0, 900.0),
                layout_mode: Some("VERTICAL".into()),
                item_spacing: Some(4.0),
                padding: Some(RawPadding {
                    top: 24.0,
                    right: 16.0,
                    bottom: 24.0,
                    left: 16.0,
                }),
                layout_sizing_vertical: Some("FILL".into()),
                children: (0..10).map(nav_item).collect(),
                ..Default::default()
            },
            RawNode {
                id: "1:3".into(),
                name: "Main content".into(),
                kind: "FRAME".into(),
                visible: true,
                absolute_bounding_box: rect(1200.0, 900.0),
                layout_mode: Some("VERTICAL".into()),
                item_spacing: Some(24.0),
                padding: Some(RawPadding {
                    top: 32.0,
                    right: 32.0,
                    bottom: 32.0,
                    left: 32.0,
                }),
                layout_sizing_horizontal: Some("FILL".into()),
                children: vec![
                    text("1:4", "Page title", "Overview", 32.0),
                    RawNode {
                        id: "1:5".into(),
                        name: "Card grid".into(),
                        kind: "FRAME".into(),
                        visible: true,
                        absolute_bounding_box: rect(1136.0, 600.0),
                        layout_mode: Some("HORIZONTAL".into()),
                        layout_wrap: Some("WRAP".into()),
                        item_spacing: Some(16.0),
                        counter_axis_spacing: Some(16.0),
                        layout_sizing_horizontal: Some("FILL".into()),
                        children: (0..24).map(card).collect(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// The file that actually exists
// ---------------------------------------------------------------------------

fn disorganised() -> RawNode {
    // Same dashboard, built by dragging. No auto layout, no tokens, no
    // components, default names, and a wrapper group around everything.
    let mut counter = 400usize;
    let mut next_id = || {
        counter += 1;
        format!("9:{counter}")
    };

    let card = |i: usize, id: &mut dyn FnMut() -> String| {
        let inner = vec![
            RawNode {
                id: id(),
                name: format!("Rectangle {}", 200 + i),
                kind: "RECTANGLE".into(),
                visible: true,
                absolute_bounding_box: rect(280.0, 140.0),
                relative_x: Some(0.0),
                relative_y: Some(0.0),
                fills: vec![literal_fill(1.0, 1.0, 1.0)],
                corner_radius: Some(RawRadius {
                    top_left: 12.0,
                    top_right: 12.0,
                    bottom_right: 12.0,
                    bottom_left: 12.0,
                }),
                ..Default::default()
            },
            {
                let mut t = text(&id(), &format!("Metric {i}"), &format!("Metric {i}"), 13.0);
                t.relative_x = Some(20.0);
                t.relative_y = Some(20.0);
                t
            },
            {
                let mut t = text(&id(), "Text", &format!("{}", (i + 1) * 1247), 28.0);
                t.relative_x = Some(20.0);
                t.relative_y = Some(48.0);
                t
            },
            // A detached instance: visually a badge, structurally a group.
            RawNode {
                id: id(),
                name: format!("Group {}", 30 + i),
                kind: "GROUP".into(),
                visible: true,
                absolute_bounding_box: rect(64.0, 24.0),
                relative_x: Some(20.0),
                relative_y: Some(96.0),
                children: vec![
                    RawNode {
                        id: id(),
                        name: "Rectangle 9".into(),
                        kind: "RECTANGLE".into(),
                        visible: true,
                        absolute_bounding_box: rect(64.0, 24.0),
                        fills: vec![literal_fill(0.85, 0.95, 0.87)],
                        ..Default::default()
                    },
                    text(&id(), "Text", "+12%", 11.0),
                ],
                ..Default::default()
            },
        ];
        // Pointless wrapper groups, two deep, as dragging produces.
        RawNode {
            id: id(),
            name: format!("Frame {}", 427 + i),
            kind: "FRAME".into(),
            visible: true,
            absolute_bounding_box: rect(280.0, 140.0),
            relative_x: Some(((i % 4) as f64) * 296.0),
            relative_y: Some(((i / 4) as f64) * 156.0),
            children: vec![RawNode {
                id: id(),
                name: format!("Group {}", 12 + i),
                kind: "GROUP".into(),
                visible: true,
                absolute_bounding_box: rect(280.0, 140.0),
                children: inner,
                ..Default::default()
            }],
            ..Default::default()
        }
    };

    let nav_item = |i: usize, id: &mut dyn FnMut() -> String| RawNode {
        id: id(),
        name: format!("Frame {}", 100 + i),
        kind: "FRAME".into(),
        visible: true,
        absolute_bounding_box: rect(200.0, 40.0),
        relative_x: Some(16.0),
        relative_y: Some(24.0 + (i as f64) * 44.0),
        children: vec![
            RawNode {
                id: id(),
                name: format!("Vector {i}"),
                kind: "VECTOR".into(),
                visible: true,
                absolute_bounding_box: rect(16.0, 16.0),
                relative_x: Some(12.0),
                relative_y: Some(12.0),
                ..Default::default()
            },
            {
                let mut t = text(&id(), "Text", &format!("Section {i}"), 14.0);
                t.relative_x = Some(40.0);
                t.relative_y = Some(11.0);
                t
            },
        ],
        ..Default::default()
    };

    let mut cards = Vec::new();
    for i in 0..24 {
        cards.push(card(i, &mut next_id));
    }
    let mut navs = Vec::new();
    for i in 0..10 {
        navs.push(nav_item(i, &mut next_id));
    }

    RawNode {
        id: "9:1".into(),
        name: "Frame 1".into(),
        kind: "FRAME".into(),
        visible: true,
        absolute_bounding_box: rect(1440.0, 900.0),
        fills: vec![literal_fill(0.97, 0.97, 0.98)],
        children: vec![
            RawNode {
                id: "9:2".into(),
                name: "Group 1".into(),
                kind: "GROUP".into(),
                visible: true,
                absolute_bounding_box: rect(240.0, 900.0),
                children: navs,
                ..Default::default()
            },
            RawNode {
                id: "9:3".into(),
                name: "Group 2".into(),
                kind: "GROUP".into(),
                visible: true,
                absolute_bounding_box: rect(1200.0, 900.0),
                relative_x: Some(240.0),
                children: vec![
                    {
                        let mut t = text("9:4", "Text", "Overview", 32.0);
                        t.relative_x = Some(32.0);
                        t.relative_y = Some(32.0);
                        t
                    },
                    RawNode {
                        id: "9:5".into(),
                        name: "Group 3".into(),
                        kind: "GROUP".into(),
                        visible: true,
                        absolute_bounding_box: rect(1136.0, 600.0),
                        relative_x: Some(32.0),
                        relative_y: Some(96.0),
                        children: cards,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

struct Report {
    nodes: usize,
    depth: usize,
    outline_tokens: usize,
    full_tokens: usize,
    flex_rules: usize,
    absolute_rules: usize,
    tokens_bound: usize,
    components: usize,
    mapped_runs: usize,
    tsx_bytes: usize,
    css_bytes: usize,
    default_names: usize,
}

fn count_default_names(n: &Node, acc: &mut usize) {
    let low = n.name.to_lowercase();
    if [
        "frame ",
        "group ",
        "rectangle ",
        "vector ",
        "ellipse ",
        "text",
    ]
    .iter()
    .any(|p| low.starts_with(p) || low == "text")
    {
        *acc += 1;
    }
    for c in &n.children {
        count_default_names(c, acc);
    }
}

fn count_tokens_bound(n: &Node, acc: &mut usize) {
    *acc += n.style.fills.iter().filter(|f| f.css_var.is_some()).count();
    for c in &n.children {
        count_tokens_bound(c, acc);
    }
}

fn analyse(raw: &RawNode) -> Report {
    let scene = ir::build(raw);
    let outline = outline::render(std::slice::from_ref(&scene), &Budget::default());
    let g = codegen::generate(
        &scene,
        &[],
        &Stack::default(),
        Some(StyleMode::CssModules),
        None,
    );

    let tsx = &g.files[0].contents;
    let css = g
        .files
        .iter()
        .find(|f| f.path.ends_with(".css"))
        .map(|f| f.contents.clone())
        .unwrap_or_default();

    let mut default_names = 0;
    count_default_names(&scene, &mut default_names);
    let mut tokens_bound = 0;
    count_tokens_bound(&scene, &mut tokens_bound);

    Report {
        nodes: scene.count(),
        depth: scene.depth(),
        outline_tokens: outline.len() / 4,
        full_tokens: serde_json::to_string(&scene).unwrap().len() / 4,
        flex_rules: css.matches("display: flex").count(),
        absolute_rules: css.matches("position: absolute").count(),
        tokens_bound,
        components: g.imported_components.len(),
        mapped_runs: tsx.matches(".map(").count(),
        tsx_bytes: tsx.len(),
        css_bytes: css.len(),
        default_names,
    }
}

fn main() {
    let o = analyse(&organised());
    let d = analyse(&disorganised());

    println!("                          organised   disorganised");
    println!("  nodes                   {:>9}   {:>12}", o.nodes, d.nodes);
    println!("  tree depth              {:>9}   {:>12}", o.depth, d.depth);
    println!();
    println!(
        "  outline tokens          {:>9}   {:>12}",
        o.outline_tokens, d.outline_tokens
    );
    println!(
        "  full-detail tokens      {:>9}   {:>12}",
        o.full_tokens, d.full_tokens
    );
    println!();
    println!(
        "  flex rules              {:>9}   {:>12}",
        o.flex_rules, d.flex_rules
    );
    println!(
        "  absolute rules          {:>9}   {:>12}",
        o.absolute_rules, d.absolute_rules
    );
    println!(
        "  token-bound fills       {:>9}   {:>12}",
        o.tokens_bound, d.tokens_bound
    );
    println!(
        "  components referenced   {:>9}   {:>12}",
        o.components, d.components
    );
    println!(
        "  .map() runs             {:>9}   {:>12}",
        o.mapped_runs, d.mapped_runs
    );
    println!(
        "  default layer names     {:>9}   {:>12}",
        o.default_names, d.default_names
    );
    println!();
    println!(
        "  generated tsx bytes     {:>9}   {:>12}",
        o.tsx_bytes, d.tsx_bytes
    );
    println!(
        "  generated css bytes     {:>9}   {:>12}",
        o.css_bytes, d.css_bytes
    );

    for (label, raw) in [("ORGANISED", organised()), ("DISORGANISED", disorganised())] {
        let scene = ir::build(&raw);
        let h = figma_canvas_mcp::health::assess(std::slice::from_ref(&scene));
        println!("\n=== {label} ({} problems) ===", h.problems());
        print!("{}", h.report());
    }
}
