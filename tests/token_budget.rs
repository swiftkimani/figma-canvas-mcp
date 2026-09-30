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

    // Cheapness must not mean uselessness: ids, nesting and semantics survive.
    assert!(
        out.contains("10:27"),
        "node ids must survive: a follow-up needs them"
    );
    assert!(out.contains("w:fill"), "FILL sizing must survive");
    assert!(
        out.contains("bg:color/surface/subtle"),
        "token names must survive"
    );
    assert!(
        out.contains("\"Item number 27\""),
        "text content must survive"
    );
    assert!(out.contains("→export"), "vectors must be flagged");
    assert!(out.contains("    "), "nesting must survive");
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
        "a 1500-node frame produced ~{t} tokens; the node budget is not capping it"
    );
    assert!(
        out.contains("not shown"),
        "truncation must be declared:\n{}",
        &out[..400]
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
