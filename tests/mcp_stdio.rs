//! Drives the compiled binary over stdio exactly as an MCP client would.
//!
//! This is the check that the server is wired up at all: the macros generated a
//! router, the tools are registered with usable schemas, and stdout carries clean
//! JSON-RPC with no log lines mixed in. Cargo builds the binary for us and hands
//! over its path in `CARGO_BIN_EXE_*`, so this runs on every platform CI covers.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

/// Send a batch of JSON-RPC lines, close stdin, and collect the replies.
fn talk(requests: &[&str]) -> Vec<serde_json::Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_figma-canvas-mcp"))
        // Port 0 lets the OS pick, so concurrent test runs never collide.
        .args(["--port", "0"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("could not spawn the server binary");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for r in requests {
            writeln!(stdin, "{r}").expect("write request");
        }
        stdin.flush().ok();
    }
    // Dropping stdin signals EOF, which ends the stdio transport cleanly.
    drop(child.stdin.take());

    let stdout = child.stdout.take().expect("stdout");
    let mut out = Vec::new();
    for line in BufReader::new(stdout).lines() {
        let line = line.expect("read stdout");
        if line.trim().is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout was not clean JSON-RPC ({e}): {line}")),
        );
    }
    let _ = child.wait();
    out
}

fn init() -> &'static str {
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"ci","version":"0"}}}"#
}

fn find(msgs: &[serde_json::Value], id: i64) -> &serde_json::Value {
    msgs.iter()
        .find(|m| m["id"].as_i64() == Some(id))
        .unwrap_or_else(|| panic!("no reply with id {id}; got {msgs:#?}"))
}

#[test]
fn initialize_announces_this_server_not_the_sdk() {
    let msgs = talk(&[init()]);
    let result = &find(&msgs, 1)["result"];

    // from_build_env() would report "rmcp" here; this must be our own name.
    assert_eq!(
        result["serverInfo"]["name"].as_str(),
        Some("figma-canvas-mcp")
    );
    assert_eq!(
        result["serverInfo"]["version"].as_str(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert!(
        result["instructions"].as_str().unwrap_or_default().len() > 100,
        "the server should tell the client how to use it"
    );
    assert!(
        result["capabilities"]["tools"].is_object(),
        "tools must be advertised"
    );
}

#[test]
fn every_tool_is_listed_with_a_description_and_schema() {
    let msgs = talk(&[
        init(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    ]);

    let tools = find(&msgs, 2)["result"]["tools"]
        .as_array()
        .expect("tools array")
        .clone();

    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for expected in [
        "figma_status",
        "get_selection",
        "read_scene",
        "get_css",
        "get_tokens",
        "get_components",
        "export_assets",
        "generate_code",
    ] {
        assert!(
            names.contains(&expected),
            "tool {expected} is missing from {names:?}"
        );
    }
    assert_eq!(names.len(), 8, "unexpected tool count: {names:?}");

    for t in &tools {
        let name = t["name"].as_str().unwrap();
        assert!(
            t["description"].as_str().unwrap_or_default().len() > 40,
            "tool {name} needs a description a model can act on"
        );
        assert_eq!(
            t["inputSchema"]["type"].as_str(),
            Some("object"),
            "tool {name} has no object input schema"
        );
    }

    // generate_code's parameters are the public API of the interesting tool.
    let generate = tools.iter().find(|t| t["name"] == "generate_code").unwrap();
    let props = generate["inputSchema"]["properties"]
        .as_object()
        .expect("generate_code properties");
    for key in [
        "node_ids",
        "style_mode",
        "component_name",
        "write",
        "include_css",
    ] {
        assert!(props.contains_key(key), "generate_code is missing `{key}`");
    }
}

#[test]
fn calling_a_tool_without_the_plugin_explains_what_to_do() {
    let msgs = talk(&[
        init(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"figma_status","arguments":{}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"read_scene","arguments":{}}}"#,
    ]);

    // figma_status is the diagnostic, so it answers rather than erroring.
    let status = find(&msgs, 3)["result"]["content"][0]["text"]
        .as_str()
        .expect("status text");
    assert!(status.contains("Not connected"), "got: {status}");
    assert!(
        status.contains("Figma Canvas Bridge"),
        "it must name the plugin to run: {status}"
    );

    // A real tool call fails, but with an actionable message.
    let scene = find(&msgs, 4);
    let text = serde_json::to_string(scene).unwrap();
    assert!(
        text.contains("No Figma plugin is connected"),
        "read_scene should explain the missing plugin: {text}"
    );
}
