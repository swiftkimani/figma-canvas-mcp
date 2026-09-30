//! `figma-canvas-mcp doctor` — check everything before blaming the tool.
//!
//! A first run has several independent things that can be wrong: the port is
//! taken, the plugin was never imported, the project root points somewhere
//! unexpected, the output directory is not writable. Diagnosing those one at a
//! time through an MCP client is slow. This checks all of them at once and
//! prints the client config to copy, so getting to a first success is one
//! command rather than a conversation.

use std::path::{Path, PathBuf};

use crate::{DEFAULT_BRIDGE_PORT, lsp, rest, stack};

const OK: &str = "ok  ";
const WARN: &str = "warn";
const FAIL: &str = "FAIL";

pub struct Options {
    pub host: String,
    pub port: u16,
    pub out_dir: PathBuf,
    pub project_root: PathBuf,
}

/// Returns false when something is wrong badly enough to stop a first run.
pub async fn run(opts: &Options) -> bool {
    let mut fatal = false;
    println!(
        "figma-canvas-mcp {}  ({} {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!();

    // --- the bridge port ---
    match tokio::net::TcpListener::bind((opts.host.as_str(), opts.port)).await {
        Ok(l) => {
            drop(l);
            println!("[{OK}] bridge port {}:{} is free", opts.host, opts.port);
        }
        Err(e) => {
            fatal = true;
            println!("[{FAIL}] cannot bind {}:{} — {e}", opts.host, opts.port);
            println!(
                "        Another copy is probably already running. Stop it, or start this one \
                 with --port <other> and change `var PORT` in plugin/ui.html and the \
                 allowedDomains in plugin/manifest.json to match."
            );
        }
    }

    // --- the plugin ---
    let plugin_dir = find_plugin_dir();
    match &plugin_dir {
        None => {
            println!("[{WARN}] plugin files not found");
            match install_dir() {
                Some(d) => println!(
                    "        Copy them somewhere stable and Figma can be pointed at them once:\n\
                     \x20         mkdir -p {}\n\
                     \x20         cp plugin/* {}/",
                    d.display(),
                    d.display()
                ),
                None => println!(
                    "        Clone the repo or download the release archive; it bundles plugin/."
                ),
            }
        }
        Some(dir) => {
            println!("[{OK}] plugin files at {}", dir.display());
            match check_plugin(dir, opts.port) {
                Ok(()) => println!(
                    "[{OK}] plugin manifest is valid and targets port {}",
                    opts.port
                ),
                Err(problem) => {
                    fatal = true;
                    println!("[{FAIL}] {problem}");
                }
            }
        }
    }

    // --- the REST route, which is the only one a browser user can take ---
    match rest::RestClient::from_env() {
        Some(_) => {
            println!("[{OK}] FIGMA_TOKEN is set — URLs can be read over REST, no plugin needed");
        }
        None => {
            println!("[{WARN}] FIGMA_TOKEN is not set");
            println!(
                "        Without it, reading requires the plugin, which Figma only offers in \
                 the Desktop app."
            );
            println!(
                "        If you work in a browser: create a token at figma.com > Settings > \
                 Security"
            );
            println!(
                "        > Personal access tokens (scope file_content:read), then set \
                 FIGMA_TOKEN."
            );
            println!("        It needs only view access to the files you read.");
        }
    }

    // --- the project whose conventions we will follow ---
    let detected = stack::detect(&opts.project_root);
    if detected.guessed {
        println!(
            "[{WARN}] no project detected at {}",
            opts.project_root.display()
        );
        println!(
            "        Generated code will default to React + TypeScript + CSS modules. \
             Pass --project-root <your repo> to match its conventions instead."
        );
    } else {
        println!("[{OK}] {}", detected.summary());
        for e in detected.evidence.iter().take(6) {
            println!("        {e}");
        }
    }

    // --- optional: a language server to verify components against ---
    match lsp::detect(&opts.project_root, detected.framework) {
        Some(spec) => println!("[{OK}] language server: {}", spec.reason),
        None => {
            println!("[{WARN}] no language server found");
            println!(
                "        Imports will follow project convention but will not be verified. \
                 Install typescript-language-server to have component paths and existence \
                 checked. Entirely optional."
            );
        }
    }

    // --- somewhere to write ---
    match writable(&opts.out_dir) {
        Ok(()) => println!(
            "[{OK}] output directory {} is writable",
            opts.out_dir.display()
        ),
        Err(e) => {
            println!("[{WARN}] cannot write to {} — {e}", opts.out_dir.display());
            println!("        generate_code and export_assets will fail until this is fixed.");
        }
    }

    println!();
    if fatal {
        println!("Fix the FAIL lines above, then run doctor again.");
    } else {
        println!("Ready. Two things left, in order:");
        println!();
        println!("  1. Register the server with your MCP client:");
        println!();
        println!("     claude mcp add figma-canvas -- {} \\", current_exe());
        println!("       --project-root {}", opts.project_root.display());
        println!();
        println!("  2. In Figma: Plugins > Development > Import plugin from manifest…");
        match &plugin_dir {
            Some(d) => println!("     and choose {}", d.join("manifest.json").display()),
            None => println!("     and choose the manifest.json from this repo's plugin/ folder"),
        }
        println!("     Then run it from Plugins > Development > Figma Canvas Bridge");
        println!("     and leave the panel open.");
        println!();
        println!("  Then ask your client to call figma_status. A green dot in the");
        println!("  plugin panel and \"connected\": true means you are done.");
    }
    !fatal
}

fn current_exe() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "figma-canvas-mcp".into())
}

/// Where an installed copy of the plugin lives.
///
/// `cargo install` copies the binary and nothing else, so an installed server
/// has no `plugin/` beside it. This is the stable path a user can point Figma's
/// file picker at once and forget about.
pub fn install_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".figma-canvas-mcp/plugin"))
}

/// Look for `plugin/` in the install location, beside the binary, then up from
/// the working directory.
fn find_plugin_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    // An installed copy takes precedence: it is the one that stays put.
    if let Some(d) = install_dir() {
        candidates.push(d);
    }
    if let Ok(exe) = std::env::current_exe() {
        // target/release/figma-canvas-mcp -> ../../plugin
        for up in 1..=3 {
            let mut p = exe.clone();
            for _ in 0..up {
                p.pop();
            }
            candidates.push(p.join("plugin"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("plugin"));
        if let Some(parent) = cwd.parent() {
            candidates.push(parent.join("plugin"));
        }
    }
    candidates
        .into_iter()
        .find(|d| d.join("manifest.json").is_file())
}

fn check_plugin(dir: &Path, port: u16) -> Result<(), String> {
    let manifest_path = dir.join("manifest.json");
    let raw = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let manifest: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("manifest.json is not valid JSON: {e}"))?;

    for key in ["name", "id", "api", "main", "ui"] {
        if manifest.get(key).and_then(|v| v.as_str()).is_none() {
            return Err(format!("manifest.json is missing \"{key}\""));
        }
    }
    for key in ["main", "ui"] {
        let file = manifest[key].as_str().unwrap_or_default();
        if !dir.join(file).is_file() {
            return Err(format!(
                "manifest.json points at plugin/{file}, which is missing"
            ));
        }
    }

    // The port is duplicated in three places and all three must agree, or the
    // socket silently never opens.
    let expected = format!("ws://127.0.0.1:{port}");
    let allowed: Vec<&str> = manifest["networkAccess"]["allowedDomains"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if !allowed.iter().any(|d| *d == expected) {
        return Err(format!(
            "manifest.json does not allow {expected} (it allows {allowed:?}). \
             Figma blocks every origin not listed, so the plugin cannot connect. \
             Add it, or run the server with --port {DEFAULT_BRIDGE_PORT}."
        ));
    }

    let ui = std::fs::read_to_string(dir.join("ui.html"))
        .map_err(|e| format!("cannot read plugin/ui.html: {e}"))?;
    if !ui.contains(&format!("var PORT = {port};")) {
        return Err(format!(
            "plugin/ui.html does not dial port {port}. Update `var PORT` to match, \
             or run the server on the port the plugin expects."
        ));
    }
    Ok(())
}

fn writable(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let probe = dir.join(".figma-canvas-mcp-write-test");
    std::fs::write(&probe, b"ok").map_err(|e| e.to_string())?;
    std::fs::remove_file(&probe).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fcm-doctor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn good_plugin(dir: &Path, port: u16) {
        std::fs::write(
            dir.join("manifest.json"),
            format!(
                r#"{{"name":"x","id":"y","api":"1.0.0","main":"code.js","ui":"ui.html",
                    "networkAccess":{{"allowedDomains":["ws://127.0.0.1:{port}"]}}}}"#
            ),
        )
        .unwrap();
        std::fs::write(dir.join("code.js"), "// code").unwrap();
        std::fs::write(
            dir.join("ui.html"),
            format!("<script>var PORT = {port};</script>"),
        )
        .unwrap();
    }

    #[test]
    fn a_correct_plugin_directory_passes() {
        let d = tmp("ok");
        good_plugin(&d, 18765);
        assert!(check_plugin(&d, 18765).is_ok());
    }

    #[test]
    fn a_port_mismatch_is_reported_with_what_to_do() {
        let d = tmp("mismatch");
        good_plugin(&d, 18765);
        let err = check_plugin(&d, 19999).unwrap_err();
        // The message has to say why it matters, not just that it differs.
        assert!(err.contains("19999"), "{err}");
        assert!(err.contains("blocks every origin"), "{err}");
    }

    #[test]
    fn a_missing_referenced_file_is_caught() {
        let d = tmp("missing");
        good_plugin(&d, 18765);
        std::fs::remove_file(d.join("code.js")).unwrap();
        let err = check_plugin(&d, 18765).unwrap_err();
        assert!(err.contains("code.js"), "{err}");
    }

    #[test]
    fn a_broken_manifest_is_named_as_such() {
        let d = tmp("broken");
        good_plugin(&d, 18765);
        std::fs::write(d.join("manifest.json"), "{not json").unwrap();
        assert!(
            check_plugin(&d, 18765)
                .unwrap_err()
                .contains("not valid JSON")
        );
    }

    #[test]
    fn writability_is_actually_tested_not_assumed() {
        let d = tmp("write");
        assert!(writable(&d.join("nested/deeper")).is_ok());
        // And the probe file does not survive the check.
        assert!(
            !d.join("nested/deeper/.figma-canvas-mcp-write-test")
                .exists()
        );
    }
}
