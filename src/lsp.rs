//! An optional Language Server client, used to verify what file-scanning can
//! only infer.
//!
//! [`stack`](crate::stack) reads `package.json` and `tsconfig.json` and concludes
//! that this project imports from `@/components/`. That is a well-informed guess
//! about *convention*. It cannot tell you whether a `Button` actually exists
//! there, or whether it accepts the `variant` prop the design implies.
//!
//! A language server can. It has already resolved the project's modules, aliases
//! and types, so asking it is both more accurate and cheaper than re-implementing
//! module resolution.
//!
//! This is enrichment, never a requirement. If no server is installed, or it
//! fails to start, or it is slow, generation proceeds on the file-scanned stack
//! and says which imports were verified and which were inferred. A tool that
//! breaks when an optional dependency is missing is not optional.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::stack::Framework;

/// How to launch a language server for a given project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    pub command: String,
    pub args: Vec<String>,
    /// For reporting: why this server was chosen.
    pub reason: String,
}

/// Candidate servers per framework, in preference order.
fn candidates(framework: Framework) -> Vec<(&'static str, &'static [&'static str])> {
    match framework {
        Framework::Vue => vec![
            ("vue-language-server", &["--stdio"]),
            ("vls", &[]),
            ("typescript-language-server", &["--stdio"]),
        ],
        Framework::Svelte => vec![
            ("svelteserver", &["--stdio"]),
            ("typescript-language-server", &["--stdio"]),
        ],
        Framework::Angular => vec![
            ("ngserver", &["--stdio"]),
            ("typescript-language-server", &["--stdio"]),
        ],
        // React, Preact, Solid and unknown projects are all TypeScript/JS.
        _ => vec![
            ("typescript-language-server", &["--stdio"]),
            ("vtsls", &["--stdio"]),
        ],
    }
}

/// Find a language server for this project.
///
/// A server installed in the project's own `node_modules/.bin` is preferred over
/// one on `PATH`: it is the version the project actually pins.
pub fn detect(root: &Path, framework: Framework) -> Option<ServerSpec> {
    for (name, args) in candidates(framework) {
        let local = root.join("node_modules/.bin").join(name);
        if local.is_file() {
            return Some(ServerSpec {
                command: local.to_string_lossy().into_owned(),
                args: args.iter().map(|s| s.to_string()).collect(),
                reason: format!("{name} from the project's node_modules"),
            });
        }
        if which(name).is_some() {
            return Some(ServerSpec {
                command: name.to_string(),
                args: args.iter().map(|s| s.to_string()).collect(),
                reason: format!("{name} on PATH"),
            });
        }
    }
    None
}

/// Locate an executable on PATH, portably.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    // Windows executables carry an extension; everything else does not.
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|e| e.to_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };

    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// A symbol the language server knows about.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    /// LSP SymbolKind. 5 = Class, 12 = Function, 13 = Variable, 23 = Struct.
    pub kind: u64,
    /// Absolute path of the file that defines it.
    pub path: PathBuf,
}

struct Inner {
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    outbound: mpsc::UnboundedSender<String>,
    next_id: AtomicU64,
    timeout: Duration,
}

pub struct LspClient {
    inner: Arc<Inner>,
    /// Held so the server is killed when the client drops.
    child: Option<tokio::process::Child>,
    pub server: String,
}

impl LspClient {
    /// Launch a server and complete the LSP handshake.
    pub async fn spawn(spec: &ServerSpec, root: &Path, timeout: Duration) -> Result<Self> {
        let mut child = tokio::process::Command::new(&spec.command)
            .args(&spec.args)
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("could not start {}", spec.command))?;

        let stdin = child.stdin.take().context("server has no stdin")?;
        let stdout = child.stdout.take().context("server has no stdout")?;

        let mut client = Self::connect(stdout, stdin, root, timeout).await?;
        client.child = Some(child);
        client.server = spec.reason.clone();
        Ok(client)
    }

    /// Complete the handshake over an arbitrary transport.
    ///
    /// Split out so tests can drive a server over an in-memory pipe rather than
    /// requiring one to be installed.
    pub async fn connect<R, W>(reader: R, writer: W, root: &Path, timeout: Duration) -> Result<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let inner = Arc::new(Inner {
            pending: Mutex::new(HashMap::new()),
            outbound: tx,
            next_id: AtomicU64::new(1),
            timeout,
        });

        // Writer: frame every message with its byte length.
        let mut writer = writer;
        tokio::spawn(async move {
            while let Some(body) = rx.recv().await {
                let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
                if writer.write_all(frame.as_bytes()).await.is_err() {
                    break;
                }
                let _ = writer.flush().await;
            }
        });

        // Reader: resolve responses by id, ignore notifications.
        let reader_inner = inner.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            while let Ok(Some(msg)) = read_frame(&mut reader).await {
                let Some(id) = msg.get("id").and_then(|v| v.as_u64()) else {
                    continue; // a notification or a server-initiated request
                };
                if let Some(tx) = reader_inner.pending.lock().await.remove(&id) {
                    let _ = tx.send(msg);
                }
            }
        });

        let client = Self {
            inner,
            child: None,
            server: "in-process".into(),
        };

        // The handshake. capabilities is intentionally minimal: we only ask
        // questions, we never open documents or request completions.
        let init = client
            .request(
                "initialize",
                json!({
                    "processId": std::process::id(),
                    "rootUri": path_to_uri(root),
                    "workspaceFolders": [{
                        "uri": path_to_uri(root),
                        "name": root.file_name().map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "root".into()),
                    }],
                    "capabilities": {
                        "workspace": { "symbol": { "dynamicRegistration": false } }
                    },
                }),
            )
            .await
            .context("the language server did not complete initialize")?;

        if init.get("error").is_some() {
            anyhow::bail!("language server rejected initialize: {init}");
        }
        client.notify("initialized", json!({}));
        Ok(client)
    }

    fn notify(&self, method: &str, params: Value) {
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let _ = self.inner.outbound.send(body.to_string());
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });

        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);
        self.inner
            .outbound
            .send(body.to_string())
            .map_err(|_| anyhow::anyhow!("language server connection closed"))?;

        match tokio::time::timeout(self.inner.timeout, rx).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(_)) => anyhow::bail!("language server closed before answering {method}"),
            Err(_) => {
                self.inner.pending.lock().await.remove(&id);
                anyhow::bail!(
                    "language server did not answer {method} within {:?}",
                    self.inner.timeout
                )
            }
        }
    }

    /// Ask the server for symbols matching a name.
    pub async fn workspace_symbol(&self, query: &str) -> Result<Vec<Symbol>> {
        let res = self
            .request("workspace/symbol", json!({ "query": query }))
            .await?;

        let items = res
            .get("result")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(items
            .iter()
            .filter_map(|s| {
                // The location is either `location.uri` or, for work-done
                // symbols, `location` reduced to just a uri.
                let uri = s
                    .pointer("/location/uri")
                    .or_else(|| s.get("uri"))
                    .and_then(|v| v.as_str())?;
                Some(Symbol {
                    name: s.get("name")?.as_str()?.to_string(),
                    kind: s.get("kind").and_then(|v| v.as_u64()).unwrap_or(0),
                    path: uri_to_path(uri)?,
                })
            })
            .collect())
    }

    /// Resolve a component name to the file that defines it.
    ///
    /// Only exported, definition-like symbols count, and the name must match
    /// exactly — `workspace/symbol` is a fuzzy search, so `Button` will happily
    /// return `ButtonGroup`, `IconButton` and `buttonStyles`.
    pub async fn find_component(&self, name: &str) -> Result<Option<Symbol>> {
        const DEFINITION_KINDS: &[u64] = &[
            5,  // Class
            11, // Interface
            12, // Function
            13, // Variable — how most React components are declared
            23, // Struct
        ];

        let symbols = self.workspace_symbol(name).await?;
        Ok(symbols
            .into_iter()
            .filter(|s| s.name == name && DEFINITION_KINDS.contains(&s.kind))
            // Prefer a definition inside the project over one in a dependency.
            .min_by_key(|s| {
                let p = s.path.to_string_lossy().to_lowercase();
                (p.contains("node_modules"), p.len())
            }))
    }

    pub async fn shutdown(mut self) {
        let _ = self.request("shutdown", json!(null)).await;
        self.notify("exit", json!(null));
        if let Some(child) = &mut self.child {
            let _ = child.start_kill();
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.start_kill();
        }
    }
}

/// Read one `Content-Length`-framed JSON-RPC message.
async fn read_frame<R: AsyncRead + Unpin>(reader: &mut BufReader<R>) -> Result<Option<Value>> {
    use tokio::io::AsyncBufReadExt;

    let mut length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None); // clean EOF
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some(v) = trimmed.strip_prefix("Content-Length:") {
            length = v.trim().parse().ok();
        }
    }

    let len = length.context("LSP frame had no Content-Length header")?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf).ok())
}

/// A filesystem path as a `file://` URI, including on Windows.
pub fn path_to_uri(path: &Path) -> String {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_string_lossy().replace('\\', "/");
    // Windows canonicalization yields a \\?\ prefix, and drive paths need a
    // leading slash: C:/x becomes file:///C:/x.
    let s = s.trim_start_matches("//?/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // file:///C:/x -> C:/x ; file:///home/x -> /home/x
    let rest = if rest.len() > 2 && rest.starts_with('/') && rest.as_bytes()[2] == b':' {
        &rest[1..]
    } else {
        rest
    };
    Some(PathBuf::from(percent_decode(rest)))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip_on_this_platform() {
        let here = std::env::current_dir().unwrap();
        let uri = path_to_uri(&here);
        assert!(uri.starts_with("file://"), "got {uri}");
        let back = uri_to_path(&uri).expect("round trip");
        assert!(
            back.ends_with(here.file_name().unwrap()),
            "{back:?} should end with {:?}",
            here.file_name().unwrap()
        );
    }

    #[test]
    fn windows_drive_uris_are_decoded() {
        let p = uri_to_path("file:///C:/Users/dev/app/Button.tsx").unwrap();
        assert_eq!(p.to_string_lossy(), "C:/Users/dev/app/Button.tsx");
    }

    #[test]
    fn percent_escapes_in_paths_are_decoded() {
        let p = uri_to_path("file:///home/dev/My%20Project/Button.tsx").unwrap();
        assert_eq!(p.to_string_lossy(), "/home/dev/My Project/Button.tsx");
    }

    #[test]
    fn a_missing_server_is_reported_not_fatal() {
        let dir = std::env::temp_dir();
        // Nothing installed under a bare temp dir.
        let found = detect(&dir, Framework::React);
        // Either None, or something genuinely on PATH — both are valid, but it
        // must never panic and must never invent a path.
        if let Some(spec) = found {
            assert!(!spec.command.is_empty());
            assert!(!spec.reason.is_empty());
        }
    }

    #[test]
    fn a_project_local_server_is_preferred_over_path() {
        let dir = std::env::temp_dir().join(format!("fcm-lsp-{}", std::process::id()));
        let bin = dir.join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("typescript-language-server");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();

        let spec = detect(&dir, Framework::React).expect("should find the local one");
        assert!(
            spec.reason.contains("node_modules"),
            "the project's pinned version should win: {}",
            spec.reason
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
