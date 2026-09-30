//! figma-canvas-mcp — read a live Figma canvas and reconstruct it as code.
//!
//! The pipeline, in order:
//!
//! 1. [`bridge`] holds a loopback WebSocket that the Figma plugin connects to.
//! 2. [`raw`] is the projection the plugin sends: Figma API field names, verbatim.
//! 3. [`ir`] normalizes that into a scene that knows parent/child relationships.
//! 4. [`css`] turns the IR into declarations, taking layout from the IR and
//!    appearance from Figma's own `getCSSAsync()`.
//! 5. [`codegen`] emits React and CSS.
//! 6. [`tools`] exposes the whole thing as MCP tools.

pub mod bridge;
pub mod codegen;
pub mod css;
pub mod ir;
pub mod motion;
pub mod outline;
pub mod protocol;
pub mod raw;
pub mod stack;
pub mod tools;

/// Default port for the plugin bridge.
///
/// This value is duplicated in `plugin/manifest.json` (`networkAccess.allowedDomains`)
/// and `plugin/ui.html` (`var PORT`), because Figma's manifest cannot read Rust
/// constants. `tests/plugin_consistency.rs` fails if they drift apart.
pub const DEFAULT_BRIDGE_PORT: u16 = 18765;
