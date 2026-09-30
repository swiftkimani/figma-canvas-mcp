//! Guards against Rust/plugin drift.
//!
//! The plugin cannot read Rust constants — Figma's manifest is static JSON and
//! `code.js` is loaded by Figma, not by us. So a handful of facts are necessarily
//! duplicated across the two sides, and every one of them is a silent runtime
//! failure when it drifts: a port mismatch means the socket never opens, a missing
//! op means a tool returns "unknown operation".
//!
//! These checks are cheap and they run on every `cargo test`, not only in CI.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use figma_canvas_mcp::DEFAULT_BRIDGE_PORT;

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
}

#[test]
fn manifest_is_valid_and_points_at_files_that_exist() {
    let manifest: serde_json::Value = serde_json::from_str(&read("plugin/manifest.json"))
        .expect("manifest.json must be valid JSON");

    for key in ["name", "id", "api", "main", "ui"] {
        assert!(
            manifest.get(key).and_then(|v| v.as_str()).is_some(),
            "manifest.json is missing required key `{key}`"
        );
    }

    for key in ["main", "ui"] {
        let file = manifest[key].as_str().unwrap();
        let path = repo("plugin").join(file);
        assert!(
            path.exists(),
            "manifest.json `{key}` points at plugin/{file}, which does not exist"
        );
    }
}

#[test]
fn manifest_allows_the_port_the_server_actually_binds() {
    let manifest: serde_json::Value = serde_json::from_str(&read("plugin/manifest.json")).unwrap();

    let domains: Vec<String> = manifest["networkAccess"]["allowedDomains"]
        .as_array()
        .expect("networkAccess.allowedDomains must be an array")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();

    // Figma blocks any origin not listed here, so a stale port is a dead socket.
    for host in ["127.0.0.1", "localhost"] {
        let expected = format!("ws://{host}:{DEFAULT_BRIDGE_PORT}");
        assert!(
            domains.contains(&expected),
            "manifest.json must allow {expected} (DEFAULT_BRIDGE_PORT is {DEFAULT_BRIDGE_PORT}); \
             it currently allows {domains:?}"
        );
    }

    // Loopback only: this server has no business accepting remote connections.
    for d in &domains {
        assert!(
            d.contains("127.0.0.1") || d.contains("localhost"),
            "manifest.json allows a non-loopback origin: {d}"
        );
    }
}

#[test]
fn plugin_ui_dials_the_same_port() {
    let ui = read("plugin/ui.html");
    let expected = format!("var PORT = {DEFAULT_BRIDGE_PORT};");
    assert!(
        ui.contains(&expected),
        "plugin/ui.html must contain `{expected}` to match DEFAULT_BRIDGE_PORT"
    );
}

/// Operation names the Rust tools send, scraped from `bridge.call("…")`.
fn ops_rust_calls() -> BTreeSet<String> {
    let src = read("src/tools.rs");
    let mut out = BTreeSet::new();
    for (idx, _) in src.match_indices(".call(\"") {
        let rest = &src[idx + ".call(\"".len()..];
        if let Some(end) = rest.find('"') {
            out.insert(rest[..end].to_string());
        }
    }
    assert!(
        !out.is_empty(),
        "found no bridge.call sites in src/tools.rs"
    );
    out
}

/// Operation names the plugin implements, scraped from the `ops` object.
fn ops_plugin_implements() -> BTreeSet<String> {
    let src = read("plugin/code.js");
    let start = src
        .find("var ops = {")
        .expect("plugin/code.js must declare `var ops = {`");
    let body = &src[start..];
    let mut out = BTreeSet::new();
    for line in body.lines() {
        let t = line.trim_start();
        // Handler declarations look like `  name: async function (p) {`.
        if let Some(colon) = t.find(": async function") {
            let name = &t[..colon];
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                out.insert(name.to_string());
            }
        }
    }
    assert!(!out.is_empty(), "found no op handlers in plugin/code.js");
    out
}

#[test]
fn every_op_the_server_calls_is_implemented_by_the_plugin() {
    let called = ops_rust_calls();
    let implemented = ops_plugin_implements();

    let missing: Vec<_> = called.difference(&implemented).collect();
    assert!(
        missing.is_empty(),
        "src/tools.rs calls op(s) plugin/code.js does not implement: {missing:?}\n\
         called:      {called:?}\n\
         implemented: {implemented:?}"
    );
}

#[test]
fn the_plugin_implements_no_dead_ops() {
    let called = ops_rust_calls();
    let implemented = ops_plugin_implements();

    let unused: Vec<_> = implemented.difference(&called).collect();
    assert!(
        unused.is_empty(),
        "plugin/code.js implements op(s) nothing calls: {unused:?} — either wire them up \
         in src/tools.rs or remove them"
    );
}

#[test]
fn dynamic_page_access_forbids_the_synchronous_node_apis() {
    let manifest: serde_json::Value = serde_json::from_str(&read("plugin/manifest.json")).unwrap();
    if manifest["documentAccess"].as_str() != Some("dynamic-page") {
        return; // the sync APIs are legal without dynamic-page
    }

    let code = read("plugin/code.js");
    // Under documentAccess: dynamic-page these throw at runtime, and the failure
    // shows up as an opaque plugin error rather than a compile problem.
    for banned in [
        "figma.getNodeById(",
        "figma.variables.getVariableById(",
        "figma.getStyleById(",
        ".mainComponent",
    ] {
        assert!(
            !code.contains(banned),
            "plugin/code.js uses `{banned}`, which is unavailable under \
             documentAccess: dynamic-page — use the Async variant"
        );
    }
}

#[test]
fn plugin_javascript_has_balanced_braces() {
    // Not a parser, just a cheap tripwire: Figma reports syntax errors as a blank
    // panel with nothing in the console, which is miserable to debug.
    let code = read("plugin/code.js");
    let mut depth = 0i32;
    let mut in_string: Option<char> = None;
    let mut prev = '\0';
    let mut in_line_comment = false;

    for ch in code.chars() {
        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
            }
            prev = ch;
            continue;
        }
        match in_string {
            Some(q) => {
                if ch == q && prev != '\\' {
                    in_string = None;
                }
            }
            None => match ch {
                '/' if prev == '/' => in_line_comment = true,
                '\'' | '"' => in_string = Some(ch),
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            },
        }
        assert!(depth >= 0, "plugin/code.js has an unmatched closing brace");
        prev = ch;
    }
    assert_eq!(depth, 0, "plugin/code.js has {depth} unclosed brace(s)");
}
