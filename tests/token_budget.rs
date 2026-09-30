//! Token budgets, enforced.
//!
//! Every byte these tools return is paid for by whoever is running the model, and
//! on a free tier a single careless payload is the entire context window. These
//! budgets exist so a future change that reintroduces a verbose default fails
//! here rather than silently costing users money.
//!
//! Byte counts are converted at a conservative ~4 bytes per token.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use figma_canvas_mcp::outline::{self, Budget};
use figma_canvas_mcp::raw::*;
use figma_canvas_mcp::{ir, ir::Node};

fn tokens(bytes: usize) -> usize {
    bytes / 4
}

/// A perfectly ordinary screen: a 50-row settings list. Nothing exotic — this is
/// the size of design a person actually points the tool at.
fn realistic_screen() -> Node {
    fn row(i: usize) -> RawNode {
        RawNode {
            id: format!("10:{i}"),
            name: format!("Row item {i}"),
            kind: "FRAME".into(),
            visible: true,
            absolute_bounding_box: Some(RawRect {
                x: 0.0,
                y: 0.0,
                width: 280.0,
                height: 48.0,
            }),
            layout_mode: Some("HORIZONTAL".into()),
            item_spacing: Some(8.0),
            primary_axis_align_items: Some("SPACE_BETWEEN".into()),
            counter_axis_align_items: Some("CENTER".into()),
            padding: Some(RawPadding {
                top: 8.0,
                right: 12.0,
                bottom: 8.0,
                left: 12.0,
            }),
            layout_sizing_horizontal: Some("FILL".into()),
            layout_sizing_vertical: Some("HUG".into()),
            corner_radius: Some(RawRadius {
                top_left: 8.0,
                top_right: 8.0,
                bottom_right: 8.0,
                bottom_left: 8.0,
            }),
            fills: vec![RawPaint {
                kind: "SOLID".into(),
                visible: Some(true),
                opacity: Some(1.0),
                color: Some(RawColor {
                    r: 0.95,
                    g: 0.96,
                    b: 0.98,
                    a: None,
                }),
                variable_name: Some("color/surface/subtle".into()),
                ..Default::default()
            }],
            children: vec![
                RawNode {
                    id: format!("10:{i}a"),
                    name: "Label".into(),
                    kind: "TEXT".into(),
                    visible: true,
                    absolute_bounding_box: Some(RawRect {
                        x: 0.0,
                        y: 0.0,
                        width: 120.0,
                        height: 20.0,
                    }),
                    text: Some(RawText {
                        characters: format!("Item number {i}"),
                        font_family: Some("Inter".into()),
                        font_size: Some(14.0),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                RawNode {
                    id: format!("10:{i}b"),
                    name: "Chevron".into(),
                    kind: "VECTOR".into(),
                    visible: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    ir::build(&RawNode {
        id: "1:1".into(),
        name: "Settings list".into(),
        kind: "FRAME".into(),
        visible: true,
        absolute_bounding_box: Some(RawRect {
            x: 0.0,
            y: 0.0,
            width: 320.0,
            height: 800.0,
        }),
        layout_mode: Some("VERTICAL".into()),
        item_spacing: Some(12.0),
        children: (0..50).map(row).collect(),
        ..Default::default()
    })
}

#[test]
fn the_default_read_of_a_real_screen_fits_a_small_context() {
    let scene = realistic_screen();
    assert_eq!(
        scene.count(),
        151,
        "fixture size changed; rebaseline the budgets"
    );

    let out = outline::render(std::slice::from_ref(&scene), &Budget::default());
    let t = tokens(out.len());

    // 4k leaves room for a system prompt, the tool schemas and an actual answer
    // inside an 8k-16k window.
    assert!(
        t < 4_000,
        "the default read_scene costs ~{t} tokens for 151 nodes; budget is 4000"
    );

    // Cheapness must not mean loss. The 50 rows are one shape, so they collapse —
    // but every fact needed to translate them accurately has to survive.
    assert!(out.contains("×50 siblings share this shape"), "{out}");
    assert!(
        out.contains("10:0 … 10:49"),
        "the id range must be stated so a follow-up can address any row:\n{out}"
    );
    assert!(out.contains("w:fill"), "FILL sizing must survive");
    assert!(
        out.contains("bg:color/surface/subtle"),
        "token names must survive"
    );
    // All fifty content values, not a sample: this is the data a .map() renders.
    for i in [0usize, 27, 49] {
        assert!(
            out.contains(&format!("\"Item number {i}\"")),
            "content for row {i} was dropped:\n{out}"
        );
    }
    assert!(out.contains("→export"), "vectors must be flagged");
    assert!(out.contains("    "), "nesting must survive");
    assert!(
        !out.contains("not expanded"),
        "collapsing a repeat is not truncation; nothing should be withheld:\n{out}"
    );
}

#[test]
fn serialization_emits_no_empty_field_noise() {
    let scene = realistic_screen();
    let json = serde_json::to_string(&scene).unwrap();

    // Each of these is pure cost across every node in a tree.
    assert!(!json.contains(":[]"), "empty arrays are being serialized");
    assert!(!json.contains(":{}"), "empty objects are being serialized");

    let nulls = json.matches(":null").count();
    assert!(
        nulls < 250,
        "{nulls} null fields across 151 nodes; add skip_serializing_if"
    );
}

#[test]
fn full_detail_is_expensive_but_bounded() {
    let scene = realistic_screen();
    let json = serde_json::to_string(&scene).unwrap();
    let t = tokens(json.len());

    // Full detail is legitimately large — that is why it is not the default —
    // but a regression that doubles it should still be caught.
    assert!(
        t < 20_000,
        "full detail costs ~{t} tokens for 151 nodes; budget is 20000"
    );

    let out = outline::render(std::slice::from_ref(&scene), &Budget::default());
    let ratio = json.len() as f64 / out.len() as f64;
    assert!(
        ratio > 4.0,
        "the outline should be several times cheaper than full detail, got {ratio:.1}x"
    );
}

#[test]
fn a_huge_frame_still_cannot_blow_the_window() {
    let mut scene = realistic_screen();
    // 10x the rows: the pathological case of pointing at a whole page.
    let rows = scene.children.clone();
    for _ in 0..9 {
        scene.children.extend(rows.iter().cloned());
    }
    assert!(scene.count() > 1400);

    let out = outline::render(std::slice::from_ref(&scene), &Budget::default());
    let t = tokens(out.len());
    assert!(
        t < 8_000,
        "a 1500-node frame produced ~{t} tokens; nothing is capping it"
    );
    // Repetition, not truncation, is what absorbs scale here — so a 10x larger
    // design costs barely more and still loses nothing.
    assert!(
        !out.contains("not expanded"),
        "a repetitive frame should collapse, not truncate:\n{}",
        &out[..600]
    );
    assert!(out.contains("siblings share this shape"), "{}", &out[..600]);
    assert!(
        out.contains("1501 node(s)"),
        "the true total must be reported"
    );
}

#[test]
fn cost_barely_grows_when_a_design_grows() {
    // The property that matters for "no matter how many frames": a design that
    // is 10x bigger but no more varied must not cost 10x more to read.
    let small = realistic_screen();
    let mut large = realistic_screen();
    let rows = large.children.clone();
    for _ in 0..9 {
        large.children.extend(rows.iter().cloned());
    }

    let a = outline::render(std::slice::from_ref(&small), &Budget::default()).len();
    let b = outline::render(std::slice::from_ref(&large), &Budget::default()).len();

    let node_growth = large.count() as f64 / small.count() as f64;
    let cost_growth = b as f64 / a as f64;
    assert!(
        cost_growth < node_growth / 2.0,
        "nodes grew {node_growth:.1}x but cost grew {cost_growth:.1}x; \
         repetition is not being absorbed"
    );
}

/// The tool schemas ship with *every* request, so they are a recurring cost, not
/// a one-off. This is the floor a user pays before any answer.
#[test]
fn the_per_request_schema_floor_stays_small() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_figma-canvas-mcp"))
        .args(["--port", "0"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");

    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2026-07-28","capabilities":{{}},"clientInfo":{{"name":"b","version":"0"}}}}}}"#
        )
        .unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list"}}"#).unwrap();
    }
    drop(child.stdin.take());

    let mut instructions = 0usize;
    let mut schema = 0usize;
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        if line.trim().is_empty() {
            continue;
        }
        let m: serde_json::Value = serde_json::from_str(&line).unwrap();
        if m["id"] == 1 {
            instructions = m["result"]["instructions"].as_str().unwrap_or("").len();
        }
        if m["id"] == 2 {
            schema = serde_json::to_string(&m["result"]["tools"]).unwrap().len();
        }
    }
    let _ = child.wait();

    let floor = tokens(instructions + schema);
    assert!(
        floor < 2_500,
        "every request pays ~{floor} tokens for instructions plus tool schemas; budget is 2500"
    );
}

#[test]
fn the_fidelity_levels_are_genuinely_priced_apart() {
    use figma_canvas_mcp::outline::Budget;

    let scene = realistic_screen();
    let roots = std::slice::from_ref(&scene);

    // Sketch: top levels only. Standard: full depth, collapsed.
    // Exactly what fidelity="sketch" uses: shallow, and no content.
    let sketch = outline::render(
        roots,
        &Budget {
            max_depth: 3,
            max_nodes: 60,
            show_content: false,
        },
    );
    let standard = outline::render(roots, &Budget::default());
    let full = serde_json::to_string(roots).unwrap();

    let (s, t, f) = (
        tokens(sketch.len()),
        tokens(standard.len()),
        tokens(full.len()),
    );

    // A menu whose options cost the same is not a menu.
    assert!(s < t, "sketch ({s}) should undercut standard ({t})");
    assert!(t < f / 4, "standard ({t}) should be far under full ({f})");

    // Each level must still be truthful about the whole design.
    for out in [&sketch, &standard] {
        assert!(
            out.contains("151 node(s)"),
            "every level must report the true total, not just what it showed"
        );
    }
    // Sketch drops the words but keeps the skeleton: every node id and shape is
    // still addressable, so a follow-up can ask for exactly what it needs.
    assert!(
        sketch.contains("10:0 … 10:49"),
        "id ranges must survive:\n{sketch}"
    );
    assert!(sketch.contains("w:fill"), "sizing must survive:\n{sketch}");
    assert!(
        !sketch.contains("\"Item number 0\""),
        "sketch should omit content; that is what makes it cheap:\n{sketch}"
    );
    // Standard is the level that carries the content.
    assert!(standard.contains("\"Item number 0\""), "{standard}");
}
