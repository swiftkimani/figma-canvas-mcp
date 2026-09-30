//! End-to-end test of the real bridge, with a fake plugin on the other end.
//!
//! This exercises the actual WebSocket server, the actual wire protocol, and the
//! actual IR and codegen path. The only thing simulated is Figma itself, which is
//! replaced by a fixture payload shaped exactly as `plugin/code.js` projects it.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use figma_canvas_mcp::bridge::Bridge;
use figma_canvas_mcp::codegen::{self, StyleMode};
use figma_canvas_mcp::ir;
use figma_canvas_mcp::raw::RawNode;

/// A card: vertical auto-layout frame, a heading that fills the width, and a
/// Button instance with variant props. The fill is bound to a variable.
fn fixture_scene() -> Value {
    json!([{
      "id": "1:1",
      "name": "Product card",
      "type": "FRAME",
      "visible": true,
      "absoluteBoundingBox": { "x": 0, "y": 0, "width": 320, "height": 180 },
      "layoutMode": "VERTICAL",
      "itemSpacing": 16,
      "counterAxisAlignItems": "CENTER",
      "padding": { "top": 24, "right": 24, "bottom": 24, "left": 24 },
      "cornerRadius": { "topLeft": 12, "topRight": 12, "bottomRight": 12, "bottomLeft": 12 },
      "fills": [{
        "type": "SOLID",
        "visible": true,
        "opacity": 1.0,
        "color": { "r": 1.0, "g": 1.0, "b": 1.0, "a": null },
        "variableId": "V:1",
        "variableName": "color/surface/raised"
      }],
      "boundVariables": { "fills": "color/surface/raised", "itemSpacing": "space/md" },
      "children": [
        {
          "id": "1:2",
          "name": "Title",
          "type": "TEXT",
          "visible": true,
          "absoluteBoundingBox": { "x": 24, "y": 24, "width": 272, "height": 40 },
          "layoutSizingHorizontal": "FILL",
          "layoutSizingVertical": "HUG",
          "text": {
            "characters": "Wireless headphones",
            "fontFamily": "Inter",
            "fontSize": 32,
            "fontWeight": 700,
            "lineHeight": "40px",
            "textAlignHorizontal": "CENTER"
          },
          "boundVariables": {},
          "children": []
        },
        {
          "id": "1:3",
          "name": "Buy now",
          "type": "INSTANCE",
          "visible": true,
          "absoluteBoundingBox": { "x": 24, "y": 80, "width": 120, "height": 40 },
          "layoutSizingHorizontal": "FIXED",
          "layoutSizingVertical": "FIXED",
          "instance": {
            "componentId": "10:5",
            "componentName": "Button/primary/md",
            "componentSetName": "Button",
            "properties": { "Variant": "primary", "Size#9:0": "md", "Loading": "false" },
            "isRemote": true
          },
          "boundVariables": {},
          "children": []
        }
      ]
    }])
}

fn fixture_variables() -> Value {
    json!([
      {
        "id": "V:1",
        "name": "color/surface/raised",
        "resolvedType": "COLOR",
        "collectionName": "Palette",
        "valuesByMode": { "Light": "#ffffff", "Dark": "#1e1e1e" },
        "description": "Card and sheet backgrounds"
      },
      {
        "id": "V:2",
        "name": "space/md",
        "resolvedType": "FLOAT",
        "collectionName": "Scale",
        "valuesByMode": { "Light": "16px" },
        "description": null
      }
    ])
}

/// Stand in for `plugin/code.js`: answer requests from the fixture.
async fn fake_plugin(addr: std::net::SocketAddr) {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .expect("fake plugin could not connect");

    ws.send(Message::Text(
        json!({
            "type": "hello",
            "env": "browser",
            "fileName": "Test file",
            "fileKey": "abc123",
            "pluginVersion": "0.1.0",
            "editorType": "figma"
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();

    while let Some(Ok(frame)) = ws.next().await {
        let Message::Text(text) = frame else { continue };
        let req: Value = serde_json::from_str(&text).unwrap();
        let id = req["id"].as_str().unwrap().to_string();
        let op = req["op"].as_str().unwrap();

        let result = match op {
            "scene" => fixture_scene(),
            "variables" => fixture_variables(),
            "selection" => json!([{ "id": "1:1", "name": "Product card", "type": "FRAME" }]),
            _ => Value::Null,
        };
        let reply = if op == "boom" {
            json!({ "type": "response", "id": id, "ok": false, "error": "node not found" })
        } else {
            json!({ "type": "response", "id": id, "ok": true, "result": result })
        };
        ws.send(Message::Text(reply.to_string().into()))
            .await
            .unwrap();
    }
}

async fn connected_bridge() -> Bridge {
    let bridge = Bridge::new(Duration::from_secs(5));
    let (listener, addr) = Bridge::bind("127.0.0.1", 0).await.expect("bind");

    tokio::spawn(bridge.clone().serve_on(listener));
    tokio::spawn(fake_plugin(addr));

    // Wait for the handshake rather than sleeping a fixed amount.
    for _ in 0..100 {
        if bridge.status().await.hello.is_some() {
            return bridge;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("fake plugin never completed the handshake");
}

#[tokio::test]
async fn reports_not_connected_before_any_plugin_attaches() {
    let bridge = Bridge::new(Duration::from_millis(200));
    assert!(!bridge.status().await.connected);
    let err = bridge.call("scene", json!({})).await.unwrap_err();
    // The message has to tell the user what to actually do.
    let msg = err.to_string();
    assert!(msg.contains("No Figma plugin is connected"), "got: {msg}");
    assert!(msg.contains("Figma Canvas Bridge"), "got: {msg}");
}

#[tokio::test]
async fn handshake_reports_the_host_and_file() {
    let bridge = connected_bridge().await;
    let status = bridge.status().await;
    assert!(status.connected);
    let hello = status.hello.unwrap();
    assert_eq!(hello.file_name, "Test file");
    assert_eq!(hello.file_key.as_deref(), Some("abc123"));
    // Browser and Desktop are both first-class; we just report which.
    assert_eq!(hello.env.to_string(), "Figma in browser");
}

#[tokio::test]
async fn scene_round_trips_into_the_ir() {
    let bridge = connected_bridge().await;
    let raw = bridge
        .call("scene", json!({ "depth": 12, "includeCss": false }))
        .await
        .expect("scene call");

    let roots: Vec<RawNode> = serde_json::from_value(raw).expect("decode scene");
    assert_eq!(roots.len(), 1);

    let node = ir::build(&roots[0]);
    assert_eq!(node.name, "Product card");
    assert_eq!(node.count(), 3);

    // Vertical auto layout became column flex with the designer's gap.
    match &node.layout {
        ir::Layout::Flex {
            direction,
            gap,
            align,
            ..
        } => {
            assert_eq!(*direction, ir::Direction::Column);
            assert_eq!(*gap, 16.0);
            assert_eq!(*align, ir::Align::Center);
        }
        other => panic!("expected column flex, got {other:?}"),
    }
    assert_eq!(node.padding.uniform(), Some(24.0));

    // The bound variable survived as a token, not a flattened hex.
    assert_eq!(
        node.style.fills[0].css_var.as_deref(),
        Some("--color-surface-raised")
    );

    // The instance kept its component identity and was not walked into.
    let button = &node.children[1];
    let inst = button.instance.as_ref().expect("instance identity");
    assert_eq!(inst.component, "Button");
    assert_eq!(inst.tag(), "Button");
    assert!(inst.from_library);
    assert!(button.children.is_empty());
}

#[tokio::test]
async fn full_pipeline_generates_usable_react() {
    let bridge = connected_bridge().await;

    let raw = bridge.call("scene", json!({ "depth": 12 })).await.unwrap();
    let roots: Vec<RawNode> = serde_json::from_value(raw).unwrap();
    let scene = ir::build(&roots[0]);

    let raw_vars = bridge.call("variables", json!({})).await.unwrap();
    let vars: Vec<figma_canvas_mcp::raw::RawVariable> = serde_json::from_value(raw_vars).unwrap();
    let tokens: Vec<ir::Token> = vars.into_iter().map(ir::Token::from).collect();

    let out = codegen::generate(&scene, &tokens, StyleMode::CssModules, None);

    let tsx = &out
        .files
        .iter()
        .find(|f| f.path.ends_with(".tsx"))
        .expect("tsx emitted")
        .contents;
    let sheet = &out
        .files
        .iter()
        .find(|f| f.path.ends_with(".module.css"))
        .expect("stylesheet emitted")
        .contents;
    let tokens_css = &out
        .files
        .iter()
        .find(|f| f.path == "tokens.css")
        .expect("tokens emitted")
        .contents;

    // The component reference, with variant props, not a pile of divs.
    assert!(tsx.contains("<Button"), "{tsx}");
    assert!(tsx.contains("variant=\"primary\""), "{tsx}");
    assert!(tsx.contains("size=\"md\""), "{tsx}");
    assert!(tsx.contains("loading={false}"), "{tsx}");
    assert_eq!(out.imported_components, vec!["Button".to_string()]);

    // Large type becomes a heading element. The exact level is a heuristic from
    // font size, so assert that it is a heading rather than pinning the level.
    assert!(
        tsx.contains("<h1") || tsx.contains("<h2") || tsx.contains("<h3"),
        "32px text should become a heading:\n{tsx}"
    );
    assert!(tsx.contains("Wireless headphones"), "{tsx}");

    // The advertised className prop is actually wired to the root element.
    assert!(tsx.contains("className].filter(Boolean)"), "{tsx}");

    // Layout is flexbox, and FILL did not become a hard-coded width.
    assert!(sheet.contains("display: flex;"), "{sheet}");
    assert!(sheet.contains("flex-direction: column;"), "{sheet}");
    assert!(sheet.contains("gap: 16px;"), "{sheet}");
    assert!(sheet.contains("padding: 24px;"), "{sheet}");
    assert!(
        sheet.contains("align-self: stretch;"),
        "FILL on the cross axis should stretch, got:\n{sheet}"
    );
    assert!(
        !sheet.contains("width: 272px"),
        "FILL leaked a fixed width:\n{sheet}"
    );

    // The fixed-size instance keeps its placement, passed to the component via
    // className, so it cannot be squashed by the flex container.
    assert!(sheet.contains("flex-shrink: 0;"), "{sheet}");
    assert!(
        tsx.contains("<Button className={styles."),
        "instance placement should reach the component:\n{tsx}"
    );

    // The root is a component boundary: Figma's canvas coordinates must not leak.
    assert!(
        !sheet.contains("position: absolute;"),
        "root must not be absolutely positioned:\n{sheet}"
    );
    assert!(!sheet.contains("left: 0px;"), "{sheet}");

    // Print the full output once so `cargo test -- --nocapture` shows what ships.
    println!(
        "=== {} ===\n{tsx}\n=== stylesheet ===\n{sheet}",
        out.component_name
    );

    // Token reference with the literal as a fallback.
    assert!(
        sheet.contains("var(--color-surface-raised, #ffffff)"),
        "{sheet}"
    );

    // Tokens carry both modes, so theming works.
    assert!(
        tokens_css.contains("--color-surface-raised: #ffffff;"),
        "{tokens_css}"
    );
    assert!(tokens_css.contains("#1e1e1e"), "{tokens_css}");
    assert!(tokens_css.contains("--space-md: 16px;"), "{tokens_css}");
}

#[tokio::test]
async fn plugin_errors_surface_verbatim() {
    let bridge = connected_bridge().await;
    let err = bridge.call("boom", json!({})).await.unwrap_err();
    assert!(err.to_string().contains("node not found"), "got: {err}");
}

#[tokio::test]
async fn requests_time_out_without_hanging_forever() {
    // A bridge with a plugin attached that never answers this op.
    let bridge = Bridge::new(Duration::from_millis(300));
    let (listener, addr) = Bridge::bind("127.0.0.1", 0).await.unwrap();
    tokio::spawn(bridge.clone().serve_on(listener));

    // A socket that connects and then goes silent.
    tokio::spawn(async move {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        ws.send(Message::Text(
            json!({
                "type": "hello", "env": "desktop", "fileName": "Silent",
                "pluginVersion": "0.1.0"
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        // Hold the connection open, answering nothing.
        std::future::pending::<()>().await;
    });

    for _ in 0..100 {
        if bridge.status().await.hello.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let err = bridge.call("scene", json!({})).await.unwrap_err();
    assert!(err.to_string().contains("did not answer"), "got: {err}");
}
