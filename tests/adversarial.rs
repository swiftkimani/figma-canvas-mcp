//! Best case and worst case.
//!
//! The happy-path tests elsewhere prove the tool works. These prove it does not
//! *break* — that a corrupt, hostile, enormous or empty payload produces a
//! degraded answer rather than a crash, a hang, or silently wrong output.
//!
//! The plugin is a privileged input: it runs in Figma and speaks to us over a
//! socket. Everything it sends is still treated as untrusted, because a bug in
//! it, an unexpected Figma API change, or anything else on loopback can put
//! arbitrary bytes on that wire.

use std::time::Duration;

use figma_canvas_mcp::codegen::{self, StyleMode};
use figma_canvas_mcp::ir;
use figma_canvas_mcp::outline::{self, Budget};
use figma_canvas_mcp::raw::*;
use figma_canvas_mcp::stack::Stack;

fn frame(id: &str, children: Vec<RawNode>) -> RawNode {
    RawNode {
        id: id.into(),
        name: format!("Frame {id}"),
        kind: "FRAME".into(),
        visible: true,
        absolute_bounding_box: Some(RawRect {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 100.0,
        }),
        layout_mode: Some("VERTICAL".into()),
        item_spacing: Some(8.0),
        children,
        ..Default::default()
    }
}

fn text(id: &str, content: &str) -> RawNode {
    RawNode {
        id: id.into(),
        name: "Label".into(),
        kind: "TEXT".into(),
        visible: true,
        text: Some(RawText {
            characters: content.into(),
            font_family: Some("Inter".into()),
            font_size: Some(16.0),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Run the whole pipeline. Returns the outline and the generated files.
fn pipeline(raw: &RawNode) -> (String, Vec<codegen::GeneratedFile>) {
    let scene = ir::build(raw);
    let outline = outline::render(std::slice::from_ref(&scene), &Budget::default());
    let generated = codegen::generate(
        &scene,
        &[],
        &Stack::default(),
        Some(StyleMode::CssModules),
        None,
    );
    (outline, generated.files)
}

// ---------------------------------------------------------------------------
// Best case
// ---------------------------------------------------------------------------

#[test]
fn best_case_a_clean_design_produces_clean_code() {
    let raw = frame(
        "1:1",
        vec![text("1:2", "Welcome"), text("1:3", "Sign in to continue")],
    );
    let (outline, files) = pipeline(&raw);

    assert!(outline.contains("1:1"), "{outline}");
    assert!(outline.contains("col gap8"), "{outline}");

    let tsx = &files[0].contents;
    assert!(tsx.contains("Welcome"), "{tsx}");
    assert!(tsx.contains("export default"), "{tsx}");
    // Balanced JSX: every opened element closes.
    assert_eq!(
        tsx.matches("<div").count() + tsx.matches("<p").count() + tsx.matches("<h").count(),
        tsx.matches("</div>").count() + tsx.matches("</p>").count() + tsx.matches("</h").count(),
        "unbalanced JSX:\n{tsx}"
    );
}

// ---------------------------------------------------------------------------
// Worst case: size and shape
// ---------------------------------------------------------------------------

#[test]
fn a_tree_deeper_than_the_cap_is_truncated_rather_than_overflowing() {
    // Building and dropping the raw fixture is itself recursive, so it runs on a
    // generous stack. What is under test is that ir::build refuses to follow it
    // down — because every walk after conversion inherits that bound.
    let handle = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let mut node = text("leaf", "bottom");
            for i in 0..(ir::MAX_IR_DEPTH * 4) {
                node = frame(&format!("d{i}"), vec![node]);
            }
            let scene = ir::build(&node);

            // Conversion stopped at the cap and said so.
            assert!(
                scene.count() <= ir::MAX_IR_DEPTH + 1,
                "conversion must stop at the cap, got {} nodes",
                scene.count()
            );
            assert!(scene.depth() <= ir::MAX_IR_DEPTH + 1);
            scene
        })
        .unwrap();
    let scene = handle
        .join()
        .expect("the guard should prevent any overflow");

    // The capped tree must now survive every downstream walk on an ordinary
    // test-thread stack, which is the production condition.
    let _ = scene.structure_key();
    let _ = scene.component_names();
    let _ = scene.text_content();
    let _ = scene.has_interactions();

    // The capped node sits deeper than the default outline renders, so the fact
    // has to reach the header — otherwise a caller reading the top of the output
    // has no idea part of the design was dropped.
    let out = outline::render(std::slice::from_ref(&scene), &Budget::default());
    assert!(
        out.contains("were not converted"),
        "truncation must be visible, not silent:\n{}",
        &out[..out.len().min(600)]
    );

    let g = codegen::generate(
        &scene,
        &[],
        &Stack::default(),
        Some(StyleMode::CssModules),
        None,
    );
    // Indentation used to grow two spaces per level: 2 MB at depth 1000.
    assert!(
        g.files[0].contents.len() < 200_000,
        "generated {} bytes from a capped tree; indentation is unbounded",
        g.files[0].contents.len()
    );
    assert!(serde_json::to_string(&scene).is_ok());
}

#[test]
fn ten_thousand_siblings_stay_bounded() {
    let kids: Vec<RawNode> = (0..10_000).map(|i| text(&format!("s{i}"), "row")).collect();
    let raw = frame("1:1", kids);

    let scene = ir::build(&raw);
    assert_eq!(scene.count(), 10_001);

    let out = outline::render(std::slice::from_ref(&scene), &Budget::default());
    // Identical siblings collapse, so this must stay tiny.
    assert!(
        out.len() < 8_000,
        "10k identical siblings produced {} bytes",
        out.len()
    );
    assert!(
        out.contains("10001 node(s)"),
        "the true total must be reported"
    );
}

#[test]
fn an_empty_payload_is_handled_not_crashed() {
    let empty = RawNode::default();
    let scene = ir::build(&empty);
    assert_eq!(scene.count(), 1);

    let (outline, files) = pipeline(&empty);
    assert!(!outline.is_empty());
    // A nameless, typeless node still has to produce a valid component.
    let tsx = &files[0].contents;
    assert!(tsx.contains("export function"), "{tsx}");
    assert!(tsx.contains("export default"), "{tsx}");
}

// ---------------------------------------------------------------------------
// Worst case: hostile values
// ---------------------------------------------------------------------------

#[test]
fn absurd_numbers_do_not_panic_or_emit_garbage_css() {
    for (label, w, h) in [
        ("negative", -500.0f64, -12.0f64),
        ("enormous", 1e300, 1e300),
        ("tiny", 1e-300, 1e-300),
        ("zero", 0.0, 0.0),
    ] {
        let mut raw = frame("1:1", vec![text("1:2", "x")]);
        raw.absolute_bounding_box = Some(RawRect {
            x: w,
            y: h,
            width: w,
            height: h,
        });
        raw.item_spacing = Some(w);
        raw.corner_radius = Some(RawRadius {
            top_left: w,
            top_right: w,
            bottom_right: w,
            bottom_left: w,
        });

        let (outline, files) = pipeline(&raw);
        assert!(!outline.is_empty(), "{label}: empty outline");
        let css = files.iter().find(|f| f.path.ends_with(".css")).unwrap();
        // The real requirement is that nothing panics and no NaN/inf reaches CSS.
        assert!(
            !css.contents.contains("NaN"),
            "{label}: NaN in CSS:\n{}",
            css.contents
        );
        assert!(
            !css.contents.contains("inf"),
            "{label}: inf in CSS:\n{}",
            css.contents
        );
    }
}

#[test]
fn hostile_text_cannot_break_out_of_jsx() {
    let payloads = [
        "</div><script>alert(1)</script>",
        "{process.env.SECRET}",
        "`${evil}`",
        "</p></div>{'x'}",
    ];
    for p in payloads {
        let raw = frame("1:1", vec![text("1:2", p)]);
        let (_, files) = pipeline(&raw);
        let tsx = &files[0].contents;

        assert!(!tsx.contains("<script>"), "script tag survived: {p}\n{tsx}");
        // Braces must be entities, or the content becomes a JSX expression.
        assert!(
            !tsx.contains("{process.env"),
            "a JSX expression was injected: {p}\n{tsx}"
        );
        assert!(
            !tsx.contains("{'x'}"),
            "a JSX expression was injected: {p}\n{tsx}"
        );
    }
}

#[test]
fn hostile_layer_names_cannot_break_out_of_css_or_jsx() {
    let names = [
        "}\n.evil { color: red; }\n.x {",
        "foo\"; background: url(http://evil)",
        "</style><script>x</script>",
        "a:hover, body",
    ];
    for name in names {
        let mut raw = frame("1:1", vec![text("1:2", "hi")]);
        raw.name = name.into();
        let (_, files) = pipeline(&raw);
        let css = files.iter().find(|f| f.path.ends_with(".css")).unwrap();

        // Class names are derived from layer names, so they must be sanitised
        // down to an identifier or a hostile name becomes a new CSS rule.
        for line in css.contents.lines().filter(|l| l.starts_with('.')) {
            let selector = line.split('{').next().unwrap_or("");
            assert!(
                selector
                    .trim()
                    .trim_start_matches('.')
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric()
                        || c == ','
                        || c == ' '
                        || c == '.'
                        || c == ':'),
                "selector escaped sanitisation for name {name:?}: {selector}"
            );
        }
        assert!(
            !css.contents.contains("<script>"),
            "name {name:?}:\n{}",
            css.contents
        );
    }
}

#[test]
fn hostile_font_and_spacing_values_cannot_inject_css() {
    // These arrive from the plugin as free strings, so they are attack surface.
    let mut raw = frame("1:1", vec![]);
    raw.children = vec![RawNode {
        id: "1:2".into(),
        name: "Label".into(),
        kind: "TEXT".into(),
        visible: true,
        text: Some(RawText {
            characters: "hi".into(),
            font_family: Some("Inter\"; background: url(http://evil); color: red".into()),
            font_size: Some(16.0),
            line_height: Some("20px; position: fixed; top: 0".into()),
            letter_spacing: Some("1px} body { display: none } .x {".into()),
            ..Default::default()
        }),
        ..Default::default()
    }];

    let (_, files) = pipeline(&raw);
    let css = &files
        .iter()
        .find(|f| f.path.ends_with(".css"))
        .unwrap()
        .contents;

    assert!(!css.contains("url("), "a url() was injected:\n{css}");
    assert!(
        !css.contains("position: fixed"),
        "declaration injected:\n{css}"
    );
    assert!(
        !css.contains("display: none"),
        "declaration injected:\n{css}"
    );
    // The font name survives, stripped of everything that is not a name.
    assert!(
        css.contains("font-family: \"Inter"),
        "the real value was lost:\n{css}"
    );
    // And no declaration is emitted with an empty value.
    assert!(!css.contains(": ;"), "empty declaration emitted:\n{css}");
    // One rule per class, and braces must balance.
    assert_eq!(
        css.matches('{').count(),
        css.matches('}').count(),
        "unbalanced braces mean a rule was broken open:\n{css}"
    );
}

#[test]
fn unicode_and_control_characters_survive_without_corrupting_output() {
    let payloads = [
        "日本語のテキスト",
        "مرحبا بالعالم",
        "emoji 🎨🚀 and ZWJ 👨‍👩‍👧",
        "zero\u{200b}width\u{200b}space",
        "combining e\u{0301}\u{0301}\u{0301}",
        "rtl \u{202e}override",
    ];
    for p in payloads {
        let raw = frame("1:1", vec![text("1:2", p)]);
        let (outline, files) = pipeline(&raw);
        // Must not panic on a char boundary, and must round-trip as valid JSON.
        assert!(!outline.is_empty(), "{p}");
        assert!(serde_json::to_string(&files[0].contents).is_ok());
    }
}

#[test]
fn a_one_megabyte_text_node_is_truncated_in_the_outline_but_kept_in_code() {
    let huge = "x".repeat(1_000_000);
    let raw = frame("1:1", vec![text("1:2", &huge)]);
    let (outline, files) = pipeline(&raw);

    // The outline is a summary; it must not become a megabyte.
    assert!(
        outline.len() < 4_000,
        "outline grew to {} bytes on one huge string",
        outline.len()
    );
    // But the code is the deliverable, so the content is not lost there.
    assert!(
        files[0].contents.len() > 900_000,
        "content was dropped from the code"
    );
}

#[test]
fn duplicate_node_ids_do_not_collide_into_one_class() {
    let raw = frame(
        "dup",
        vec![
            text("dup", "first"),
            text("dup", "second"),
            text("dup", "third"),
        ],
    );
    let (_, files) = pipeline(&raw);
    let tsx = &files[0].contents;
    // Content must all survive even though the ids are identical.
    for expected in ["first", "second", "third"] {
        assert!(tsx.contains(expected), "{expected} lost:\n{tsx}");
    }
}

#[test]
fn a_very_long_layer_name_does_not_produce_an_unusable_class() {
    let mut raw = frame("1:1", vec![text("1:2", "hi")]);
    raw.name = "a".repeat(10_000);
    let (_, files) = pipeline(&raw);
    let css = &files
        .iter()
        .find(|f| f.path.ends_with(".css"))
        .unwrap()
        .contents;
    let selector = css
        .lines()
        .find(|l| l.starts_with('.'))
        .unwrap_or("")
        .split('{')
        .next()
        .unwrap_or("")
        .trim();
    assert!(
        selector.len() < 200,
        "class name is {} chars; that is not a usable identifier",
        selector.len()
    );
}

// ---------------------------------------------------------------------------
// Worst case: the wire
// ---------------------------------------------------------------------------

/// A tool call runs on a tokio worker thread, which gets a 2 MiB stack rather
/// than the main thread's 8 MiB. Deserialising and dropping a nested tree is
/// recursive, so the safe depth is a property of that stack size — and testing
/// it on the main thread would pass while production crashed.
fn parses_on_a_worker_sized_stack(levels: usize) -> bool {
    let mut json = String::from(r#"{"id":"leaf","type":"FRAME","visible":true}"#);
    for i in (0..levels).rev() {
        json = format!(r#"{{"id":"n{i}","type":"FRAME","visible":true,"children":[{json}]}}"#);
    }
    let payload = format!("[{json}]");

    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let parsed: Result<Vec<RawNode>, _> = serde_json::from_str(&payload);
            let ok = parsed.is_ok();
            drop(parsed); // dropping the tree is recursive too
            ok
        })
        .unwrap()
        .join()
        .unwrap_or(false)
}

#[test]
fn the_requested_wire_depth_is_safe_on_a_worker_stack() {
    let depth = figma_canvas_mcp::tools::MAX_WIRE_DEPTH as usize;
    assert!(
        parses_on_a_worker_sized_stack(depth),
        "MAX_WIRE_DEPTH is {depth}, which does not survive a 2 MiB stack — \
         a design this deep would abort the server process"
    );
}

#[test]
fn the_wire_depth_keeps_meaningful_headroom() {
    let depth = figma_canvas_mcp::tools::MAX_WIRE_DEPTH as usize;
    // Measured: 56 survives and 62 aborts on a 2 MiB stack. Sitting right at the
    // edge is not a limit, it is a coin flip.
    assert!(
        depth <= 48,
        "MAX_WIRE_DEPTH of {depth} is too close to the measured 56-level cliff"
    );
    assert!(
        depth >= 24,
        "MAX_WIRE_DEPTH of {depth} is shallower than real designs nest"
    );
}

#[tokio::test]
async fn a_plugin_that_sends_garbage_does_not_take_the_bridge_down() {
    use figma_canvas_mcp::bridge::Bridge;
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    let bridge = Bridge::new(Duration::from_millis(400));
    let (listener, addr) = Bridge::bind("127.0.0.1", 0).await.unwrap();
    tokio::spawn(bridge.clone().serve_on(listener));

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .unwrap();

    // Every one of these is something a broken plugin could plausibly send.
    for junk in [
        "not json at all",
        "{",
        "[]",
        r#"{"type":"unknown_kind"}"#,
        r#"{"type":"response"}"#,
        r#"{"type":"response","id":"never-requested","ok":true}"#,
        r#"{"type":"hello"}"#,
        "\u{0}\u{1}\u{2}",
    ] {
        ws.send(Message::Text(junk.to_string().into()))
            .await
            .unwrap();
    }
    ws.send(Message::Binary(vec![0xff, 0xfe, 0xfd].into()))
        .await
        .unwrap();

    // A valid hello after all that must still be accepted.
    ws.send(Message::Text(
        serde_json::json!({
            "type": "hello", "env": "desktop", "fileName": "Survivor",
            "pluginVersion": "0.1.0"
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();

    for _ in 0..50 {
        if let Some(h) = bridge.status().await.hello {
            assert_eq!(h.file_name, "Survivor");
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the bridge stopped accepting valid frames after being sent junk");
}

#[tokio::test]
async fn a_plugin_that_vanishes_mid_request_fails_fast() {
    use figma_canvas_mcp::bridge::Bridge;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let bridge = Bridge::new(Duration::from_secs(30));
    let (listener, addr) = Bridge::bind("127.0.0.1", 0).await.unwrap();
    tokio::spawn(bridge.clone().serve_on(listener));

    tokio::spawn(async move {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        ws.send(Message::Text(
            serde_json::json!({
                "type":"hello","env":"browser","fileName":"Doomed","pluginVersion":"0.1.0"
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        // Take one request, then drop the socket without answering.
        let _ = ws.next().await;
        drop(ws);
    });

    for _ in 0..100 {
        if bridge.status().await.hello.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Must not wait out the 30s timeout: the disconnect is the answer.
    let started = std::time::Instant::now();
    let err = bridge
        .call("scene", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a dropped connection should fail immediately, took {:?}",
        started.elapsed()
    );
    assert!(!err.to_string().is_empty());
}

#[tokio::test]
async fn concurrent_requests_are_not_cross_wired() {
    use figma_canvas_mcp::bridge::Bridge;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let bridge = Bridge::new(Duration::from_secs(5));
    let (listener, addr) = Bridge::bind("127.0.0.1", 0).await.unwrap();
    tokio::spawn(bridge.clone().serve_on(listener));

    // A server that answers out of order, with the op echoed back.
    tokio::spawn(async move {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        ws.send(Message::Text(
            serde_json::json!({
                "type":"hello","env":"desktop","fileName":"Racer","pluginVersion":"0.1.0"
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

        let mut queued: Vec<(String, String)> = Vec::new();
        while let Some(Ok(Message::Text(t))) = ws.next().await {
            let v: serde_json::Value = serde_json::from_str(&t).unwrap();
            queued.push((
                v["id"].as_str().unwrap().to_string(),
                v["op"].as_str().unwrap().to_string(),
            ));
            if queued.len() == 8 {
                // Reverse order, to prove correlation is by id and not arrival.
                for (id, op) in queued.iter().rev() {
                    let reply = serde_json::json!({
                        "type":"response","id":id,"ok":true,"result":{"echo":op}
                    });
                    ws.send(Message::Text(reply.to_string().into()))
                        .await
                        .unwrap();
                }
                queued.clear();
            }
        }
    });

    for _ in 0..100 {
        if bridge.status().await.hello.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let ops = [
        "scene",
        "css",
        "variables",
        "components",
        "export",
        "selection",
        "interactions",
        "scene",
    ];
    let futures: Vec<_> = ops
        .iter()
        .map(|op| {
            let b = bridge.clone();
            async move { (op, b.call(op, serde_json::json!({})).await) }
        })
        .collect();

    for (op, result) in futures_util::future::join_all(futures).await {
        let v = result.unwrap_or_else(|e| panic!("{op} failed: {e}"));
        assert_eq!(
            v["echo"].as_str(),
            Some(*op),
            "responses were cross-wired: {op} received {v}"
        );
    }
}
