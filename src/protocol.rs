//! Wire protocol spoken between the Rust server and the Figma plugin.
//!
//! One socket, two directions. The server sends [`BridgeRequest`]; the plugin
//! answers with a [`PluginMessage::Response`] carrying the same `id`. Everything
//! is correlated by that id, so requests may be in flight concurrently.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where the plugin is running. Both are fully supported; the distinction only
/// affects what we can tell the user about troubleshooting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginEnv {
    /// Figma Desktop app.
    Desktop,
    /// Figma in a browser tab.
    Browser,
    /// Plugin could not determine the host.
    Unknown,
}

impl std::fmt::Display for PluginEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            PluginEnv::Desktop => "Figma Desktop",
            PluginEnv::Browser => "Figma in browser",
            PluginEnv::Unknown => "unknown host",
        };
        f.write_str(s)
    }
}

/// Sent by the plugin immediately after the socket opens.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub env: PluginEnv,
    pub file_name: String,
    #[serde(default)]
    pub file_key: Option<String>,
    pub plugin_version: String,
    /// "figma" or "dev" (Dev Mode). Figma exposes no reliable read-only flag, so
    /// we report what it does tell us rather than infer.
    #[serde(default)]
    pub editor_type: Option<String>,
}

/// Server -> plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeRequest {
    pub id: String,
    pub op: String,
    #[serde(default)]
    pub params: Value,
}

/// Plugin -> server, in reply to a [`BridgeRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeResponse {
    pub id: String,
    pub ok: bool,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Anything the plugin may send us.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginMessage {
    Hello(Hello),
    Response(BridgeResponse),
    /// Pushed on selection change so `figma_status` is current without polling.
    SelectionChanged {
        #[serde(default)]
        node_ids: Vec<String>,
    },
    /// Plugin-side diagnostics, surfaced in our tracing output.
    Log {
        level: String,
        message: String,
    },
}
