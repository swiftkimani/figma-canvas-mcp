//! Drives the real LSP client against a fake language server.
//!
//! The point is to cover the protocol — framing, the handshake, request
//! correlation, symbol filtering — without requiring anyone to install
//! typescript-language-server to run the test suite. The transport is an
//! in-memory pipe, so everything except the child process is genuine.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use figma_canvas_mcp::lsp::{LspClient, path_to_uri};

/// Read one Content-Length framed message, exactly as a real server would.
async fn read_frame<R: tokio::io::AsyncRead + Unpin>(r: &mut BufReader<R>) -> Option<Value> {
    let mut len = 0usize;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let t = line.trim_end_matches(['\r', '\n']);
        if t.is_empty() {
            break;
        }
        if let Some(v) = t.strip_prefix("Content-Length:") {
            len = v.trim().parse().ok()?;
        }
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await.ok()?;
    serde_json::from_slice(&buf).ok()
}

/// A fake server that answers `initialize` and one fixed `workspace/symbol` set.
///
/// `symbols` is what it returns for every query, so the test can check that the
/// *client* does the filtering rather than trusting the server.
fn fake_server(stream: tokio::io::DuplexStream, symbols: Vec<Value>) {
    tokio::spawn(async move {
        let (r, mut w) = tokio::io::split(stream);
        let mut r = BufReader::new(r);

        let send = |v: Value| -> Vec<u8> {
            let body = v.to_string();
            format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
        };

        while let Some(msg) = read_frame(&mut r).await {
            let Some(id) = msg.get("id").and_then(|v| v.as_u64()) else {
                continue; // notification
            };
            let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let reply = match method {
                "initialize" => json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "capabilities": { "workspaceSymbolProvider": true } }
                }),
                "workspace/symbol" => {
                    json!({ "jsonrpc": "2.0", "id": id, "result": symbols })
                }
                _ => json!({ "jsonrpc": "2.0", "id": id, "result": null }),
            };
            if w.write_all(&send(reply)).await.is_err() {
                break;
            }
            let _ = w.flush().await;
        }
    });
}

fn symbol(name: &str, kind: u64, path: &str) -> Value {
    json!({
        "name": name,
        "kind": kind,
        "location": { "uri": format!("file://{path}"), "range": {
            "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}
        }}
    })
}

async fn client_with(symbols: Vec<Value>) -> LspClient {
    let (client_end, server_end) = tokio::io::duplex(64 * 1024);
    fake_server(server_end, symbols);
    let (r, w) = tokio::io::split(client_end);
    LspClient::connect(
        r,
        w,
        &std::env::current_dir().unwrap(),
        Duration::from_secs(5),
    )
    .await
    .expect("handshake should complete")
}

#[tokio::test]
async fn the_handshake_completes_over_real_framing() {
    let c = client_with(vec![]).await;
    // Getting here at all means initialize was framed, sent, correlated by id,
    // and answered.
    assert_eq!(c.workspace_symbol("anything").await.unwrap().len(), 0);
}

#[tokio::test]
async fn a_component_resolves_to_the_file_that_defines_it() {
    let c = client_with(vec![symbol("Button", 13, "/app/src/components/Button.tsx")]).await;

    let found = c.find_component("Button").await.unwrap().expect("Button");
    assert_eq!(found.name, "Button");
    assert_eq!(
        found.path,
        PathBuf::from("/app/src/components/Button.tsx"),
        "the real definition path is the whole point"
    );
}

#[tokio::test]
async fn fuzzy_matches_are_rejected() {
    // workspace/symbol is a fuzzy search: asking for "Button" returns anything
    // containing it. Accepting those would generate imports for components that
    // do not exist under the name we are about to write.
    let c = client_with(vec![
        symbol("ButtonGroup", 13, "/app/src/ButtonGroup.tsx"),
        symbol("IconButton", 13, "/app/src/IconButton.tsx"),
        symbol("buttonStyles", 13, "/app/src/styles.ts"),
    ])
    .await;

    assert!(
        c.find_component("Button").await.unwrap().is_none(),
        "no exact match means the component does not exist"
    );
}

#[tokio::test]
async fn a_projects_own_component_beats_one_in_node_modules() {
    let c = client_with(vec![
        symbol("Button", 13, "/app/node_modules/@mui/material/Button.d.ts"),
        symbol("Button", 13, "/app/src/components/Button.tsx"),
    ])
    .await;

    let found = c.find_component("Button").await.unwrap().unwrap();
    assert_eq!(
        found.path,
        PathBuf::from("/app/src/components/Button.tsx"),
        "a dependency's Button is not this project's Button"
    );
}

#[tokio::test]
async fn non_definition_symbols_are_ignored() {
    // Kind 6 is Method, 7 is Property — a `Button` property on some config
    // object is not a component.
    let c = client_with(vec![
        symbol("Button", 6, "/app/src/theme.ts"),
        symbol("Button", 7, "/app/src/tokens.ts"),
    ])
    .await;
    assert!(c.find_component("Button").await.unwrap().is_none());
}

#[tokio::test]
async fn a_silent_server_times_out_rather_than_hanging() {
    let (client_end, server_end) = tokio::io::duplex(8192);

    // A server that completes the handshake and then stops answering.
    tokio::spawn(async move {
        let (r, mut w) = tokio::io::split(server_end);
        let mut r = BufReader::new(r);
        if let Some(msg) = read_frame(&mut r).await {
            let id = msg["id"].as_u64().unwrap();
            let body = json!({"jsonrpc":"2.0","id":id,"result":{"capabilities":{}}}).to_string();
            let _ = w
                .write_all(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes())
                .await;
            let _ = w.flush().await;
        }
        std::future::pending::<()>().await;
    });

    let (r, w) = tokio::io::split(client_end);
    let c = LspClient::connect(
        r,
        w,
        &std::env::current_dir().unwrap(),
        Duration::from_millis(300),
    )
    .await
    .expect("handshake");

    let err = c.workspace_symbol("Button").await.unwrap_err();
    assert!(
        err.to_string().contains("did not answer"),
        "a hung language server must not hang code generation: {err}"
    );
}

#[test]
fn uris_are_well_formed_for_this_platform() {
    let uri = path_to_uri(&std::env::current_dir().unwrap());
    assert!(uri.starts_with("file:///"), "got {uri}");
    assert!(!uri.contains('\\'), "URIs use forward slashes: {uri}");
}
