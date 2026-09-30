//! The loopback WebSocket bridge to the Figma plugin.
//!
//! Why a socket at all: a Figma plugin's sandbox has no network access. Only the
//! plugin's `ui.html` iframe can open a connection, and it relays to the sandbox
//! over `postMessage`. That indirection is also what makes this work unchanged in
//! Figma Desktop and in a browser tab — the iframe is the same in both.
//!
//! We bind loopback only. Chrome and Edge treat `ws://127.0.0.1` as a
//! potentially-trustworthy origin, so a page on `https://figma.com` is allowed to
//! connect without tripping mixed-content blocking.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::protocol::{BridgeRequest, BridgeResponse, Hello, PluginMessage};

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error(
        "No Figma plugin is connected. Open your file in Figma (Desktop or browser), \
         run Plugins > Development > Figma Canvas Bridge, and leave the panel open."
    )]
    NotConnected,

    #[error(
        "The plugin did not answer within {0:?}. A very large selection can exceed this; \
         select a single frame, or raise --request-timeout."
    )]
    Timeout(Duration),

    #[error("Figma plugin reported: {0}")]
    Plugin(String),

    #[error("Bridge transport failure: {0}")]
    Transport(String),
}

impl From<BridgeError> for rmcp::ErrorData {
    fn from(e: BridgeError) -> Self {
        rmcp::ErrorData::internal_error(e.to_string(), None)
    }
}

/// Live connection state. Exactly one plugin is attached at a time; a new
/// connection supersedes the old one, which is what you want when Figma reloads
/// the plugin on edit.
struct Session {
    outbound: mpsc::UnboundedSender<Message>,
    hello: Option<Hello>,
    selection: Vec<String>,
}

struct Inner {
    session: Mutex<Option<Session>>,
    pending: Mutex<HashMap<String, oneshot::Sender<BridgeResponse>>>,
    timeout: Duration,
}

#[derive(Clone)]
pub struct Bridge {
    inner: Arc<Inner>,
}

/// A snapshot of bridge state, for `figma_status`.
#[derive(Debug, Clone)]
pub struct BridgeStatus {
    pub connected: bool,
    pub hello: Option<Hello>,
    pub selection: Vec<String>,
}

impl Bridge {
    pub fn new(timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                session: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
                timeout,
            }),
        }
    }

    pub async fn status(&self) -> BridgeStatus {
        let guard = self.inner.session.lock().await;
        match guard.as_ref() {
            Some(s) => BridgeStatus {
                connected: true,
                hello: s.hello.clone(),
                selection: s.selection.clone(),
            },
            None => BridgeStatus {
                connected: false,
                hello: None,
                selection: Vec::new(),
            },
        }
    }

    /// Bind the bridge socket, returning the listener and the address actually
    /// bound. Port 0 yields an OS-assigned port, which is what tests use.
    pub async fn bind(host: &str, port: u16) -> anyhow::Result<(TcpListener, SocketAddr)> {
        let requested = format!("{host}:{port}");
        let listener = TcpListener::bind(&requested)
            .await
            .with_context(|| format!("could not bind {requested} (is another instance running?)"))?;
        let addr = listener.local_addr().context("listener has no local address")?;
        Ok((listener, addr))
    }

    /// Bind and serve plugin connections until cancelled.
    pub async fn serve(self, host: &str, port: u16) -> anyhow::Result<()> {
        let (listener, addr) = Self::bind(host, port).await?;
        tracing::info!("bridge listening on ws://{addr}");
        self.serve_on(listener).await
    }

    /// Serve on a listener that is already bound.
    pub async fn serve_on(self, listener: TcpListener) -> anyhow::Result<()> {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("accept failed: {e}");
                    continue;
                }
            };
            let bridge = self.clone();
            tokio::spawn(async move {
                if let Err(e) = bridge.handle(stream).await {
                    tracing::info!("plugin session from {peer} ended: {e}");
                }
                bridge.drop_session().await;
            });
        }
    }

    async fn handle(&self, stream: TcpStream) -> anyhow::Result<()> {
        let ws = tokio_tungstenite::accept_async(stream)
            .await
            .context("websocket handshake failed")?;
        let (mut sink, mut source) = ws.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

        // Install the session before reading, so a request racing the Hello still
        // has somewhere to go.
        {
            let mut guard = self.inner.session.lock().await;
            *guard = Some(Session {
                outbound: tx,
                hello: None,
                selection: Vec::new(),
            });
        }
        tracing::info!("plugin connected");

        let writer = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
        });

        while let Some(frame) = source.next().await {
            let frame = frame.context("websocket read failed")?;
            let text = match frame {
                Message::Text(t) => t.to_string(),
                Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                Message::Close(_) => break,
                _ => continue,
            };
            self.dispatch(&text).await;
        }

        writer.abort();
        Ok(())
    }

    async fn dispatch(&self, text: &str) {
        let msg: PluginMessage = match serde_json::from_str(text) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("undecodable plugin frame ({e}): {}", truncate(text, 240));
                return;
            }
        };

        match msg {
            PluginMessage::Hello(hello) => {
                tracing::info!(
                    "plugin ready: {} in {} (file {:?}, editor {:?})",
                    hello.plugin_version,
                    hello.env,
                    hello.file_name,
                    hello.editor_type
                );
                if let Some(s) = self.inner.session.lock().await.as_mut() {
                    s.hello = Some(hello);
                }
            }
            PluginMessage::SelectionChanged { node_ids } => {
                if let Some(s) = self.inner.session.lock().await.as_mut() {
                    s.selection = node_ids;
                }
            }
            PluginMessage::Log { level, message } => {
                tracing::info!("plugin[{level}]: {message}");
            }
            PluginMessage::Response(resp) => {
                let waiter = self.inner.pending.lock().await.remove(&resp.id);
                match waiter {
                    Some(tx) => {
                        let _ = tx.send(resp);
                    }
                    None => tracing::warn!("response for unknown request id {}", resp.id),
                }
            }
        }
    }

    async fn drop_session(&self) {
        *self.inner.session.lock().await = None;
        // Fail anything still waiting rather than let it hit the timeout.
        self.inner.pending.lock().await.clear();
        tracing::info!("plugin disconnected");
    }

    /// Issue one operation to the plugin and await its reply.
    pub async fn call(&self, op: &str, params: Value) -> Result<Value, BridgeError> {
        let id = uuid::Uuid::new_v4().to_string();
        let req = BridgeRequest {
            id: id.clone(),
            op: op.to_string(),
            params,
        };
        let payload = serde_json::to_string(&req)
            .map_err(|e| BridgeError::Transport(format!("could not encode request: {e}")))?;

        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id.clone(), tx);

        // Send while holding only the session lock.
        {
            let guard = self.inner.session.lock().await;
            let Some(session) = guard.as_ref() else {
                self.inner.pending.lock().await.remove(&id);
                return Err(BridgeError::NotConnected);
            };
            if session.outbound.send(Message::Text(payload.into())).is_err() {
                self.inner.pending.lock().await.remove(&id);
                return Err(BridgeError::NotConnected);
            }
        }

        match tokio::time::timeout(self.inner.timeout, rx).await {
            Ok(Ok(resp)) => {
                if resp.ok {
                    Ok(resp.result.unwrap_or(Value::Null))
                } else {
                    Err(BridgeError::Plugin(
                        resp.error.unwrap_or_else(|| "unspecified error".into()),
                    ))
                }
            }
            Ok(Err(_)) => Err(BridgeError::NotConnected),
            Err(_) => {
                self.inner.pending.lock().await.remove(&id);
                Err(BridgeError::Timeout(self.inner.timeout))
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\u{2026}", &s[..end])
}
