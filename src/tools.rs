//! The MCP tool surface.
//!
//! Each tool maps to one capability the Plugin API gives us and the REST API does
//! not, or does worse. Tools return pretty JSON or source text, because that is
//! what a model actually reads.

use std::path::PathBuf;

use base64::Engine as _;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::bridge::Bridge;
use crate::codegen::{self, StyleMode};
use crate::ir::{self, Token};
use crate::motion::{self, Action, Interaction};
use crate::outline::{self, Budget};
use crate::raw::{RawNode, RawVariable};

fn bad(msg: impl Into<String>) -> ErrorData {
    ErrorData::internal_error(msg.into(), None)
}

/// Serialize for a model's context window.
///
/// Indentation is roughly half the bytes of a deeply nested payload, so anything
/// large is emitted compact. Small results stay pretty, where legibility is free.
fn pretty<T: Serialize>(v: &T) -> Result<String, ErrorData> {
    let compact =
        serde_json::to_string(v).map_err(|e| bad(format!("could not serialize result: {e}")))?;
    if compact.len() > PRETTY_LIMIT {
        return Ok(compact);
    }
    serde_json::to_string_pretty(v).map_err(|e| bad(format!("could not serialize result: {e}")))
}

/// Above this many bytes, drop the indentation.
const PRETTY_LIMIT: usize = 2048;

// ---------------------------------------------------------------------------
// Tool parameters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Detail {
    /// One line per node: ids, nesting, layout, sizing, components, text.
    /// A small fraction of `full`'s size, and enough to decide what to inspect.
    #[default]
    Outline,
    /// The complete scene model as JSON. Use on a subtree, not a whole page.
    Full,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadSceneArgs {
    /// Figma node ids to read. Omit to use the current selection in Figma.
    #[serde(default)]
    pub node_ids: Option<Vec<String>>,
    /// "outline" (default, cheap) or "full" (complete JSON, expensive).
    #[serde(default)]
    pub detail: Option<Detail>,
    /// Levels to show. Default 12.
    #[serde(default)]
    pub depth: Option<u32>,
    /// Cap on nodes returned. Default 300; the result says what was omitted.
    #[serde(default)]
    pub max_nodes: Option<u32>,
    /// Also fetch Figma's own getCSSAsync() per node. Exact, but much larger.
    #[serde(default)]
    pub include_css: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NodesArgs {
    /// Figma node ids. Omit to use the current selection.
    #[serde(default)]
    pub node_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExportArgs {
    /// Figma node ids to export. Omit to use the current selection.
    #[serde(default)]
    pub node_ids: Option<Vec<String>>,
    /// SVG, PNG, JPG or PDF. Default SVG for vectors, PNG otherwise.
    #[serde(default)]
    pub format: Option<String>,
    /// Raster scale factor, e.g. 2 for @2x. Ignored for SVG. Default 2.
    #[serde(default)]
    pub scale: Option<f64>,
    /// Directory to write into. Defaults to the server's --out-dir.
    #[serde(default)]
    pub out_dir: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GenerateArgs {
    /// Figma node ids. Omit to use the current selection.
    #[serde(default)]
    pub node_ids: Option<Vec<String>>,
    /// How styles are emitted. Default css-modules.
    #[serde(default)]
    pub style_mode: Option<StyleMode>,
    /// Override the generated component name. Defaults to the frame's name.
    #[serde(default)]
    pub component_name: Option<String>,
    /// Write the files to disk under --out-dir instead of only returning them.
    #[serde(default)]
    pub write: Option<bool>,
    /// Fetch Figma's getCSSAsync() for exact paint and type. Default true.
    #[serde(default)]
    pub include_css: Option<bool>,
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct FigmaServer {
    bridge: Bridge,
    out_dir: PathBuf,
}

#[tool_router]
impl FigmaServer {
    pub fn new(bridge: Bridge, out_dir: PathBuf) -> Self {
        Self { bridge, out_dir }
    }

    async fn nodes_param(&self, ids: Option<Vec<String>>) -> Value {
        match ids {
            Some(v) if !v.is_empty() => json!({ "nodeIds": v }),
            _ => json!({ "nodeIds": Value::Null }),
        }
    }

    /// Fetch the raw tree and normalize it.
    async fn scene(
        &self,
        ids: Option<Vec<String>>,
        depth: u32,
        include_css: bool,
    ) -> Result<Vec<ir::Node>, ErrorData> {
        let mut params = self.nodes_param(ids).await;
        params["depth"] = json!(depth);
        params["includeCss"] = json!(include_css);

        let raw = self.bridge.call("scene", params).await?;
        let roots: Vec<RawNode> = serde_json::from_value(raw)
            .map_err(|e| bad(format!("plugin sent a scene we could not decode: {e}")))?;
        Ok(roots.iter().map(ir::build).collect())
    }

    async fn tokens(&self) -> Result<Vec<Token>, ErrorData> {
        let raw = self.bridge.call("variables", json!({})).await?;
        let vars: Vec<RawVariable> = serde_json::from_value(raw)
            .map_err(|e| bad(format!("plugin sent variables we could not decode: {e}")))?;
        Ok(vars.into_iter().map(Token::from).collect())
    }

    /// Is a plugin attached, where is it running, and what is selected?
    ///
    /// Call this first when anything else fails — it distinguishes "plugin not
    /// open" from "wrong node id".
    #[tool(
        description = "Check the Figma bridge: whether the plugin is connected, whether it is \
                       running in Figma Desktop or a browser tab, which file is open, and what \
                       is currently selected."
    )]
    async fn figma_status(&self) -> Result<String, ErrorData> {
        let s = self.bridge.status().await;
        if !s.connected {
            return Ok(concat!(
                "Not connected.\n\n",
                "Open your file in Figma (Desktop app or a browser tab), then run\n",
                "  Plugins > Development > Figma Canvas Bridge\n",
                "and leave the plugin panel open. It reconnects on its own.\n\n",
                "Browser note: Chrome and Edge allow ws://127.0.0.1 from https://figma.com. \
                 Safari and Firefox are stricter about loopback WebSockets — use the Desktop \
                 app there."
            )
            .to_string());
        }

        let h = s.hello.as_ref();
        pretty(&json!({
            "connected": true,
            "host": h.map(|h| h.env.to_string()),
            "file": h.map(|h| h.file_name.clone()),
            "fileKey": h.and_then(|h| h.file_key.clone()),
            "editorType": h.and_then(|h| h.editor_type.clone()),
            "pluginVersion": h.map(|h| h.plugin_version.clone()),
            "selection": s.selection,
            "selectionCount": s.selection.len(),
        }))
    }

    /// Cheap orientation before pulling a whole tree.
    #[tool(
        description = "List what is selected in Figma right now: id, name, type and size for each \
                       node. Cheap — use it to orient before reading a full scene."
    )]
    async fn get_selection(&self) -> Result<String, ErrorData> {
        let v = self.bridge.call("selection", json!({})).await?;
        pretty(&v)
    }

    /// The reconstruction: a normalized, code-ready model of the canvas.
    #[tool(
        description = "Read the canvas: nesting, auto-layout as flexbox semantics, sizing, \
                       token-named paint, text, and component/variant identity. Defaults to a \
                       cheap one-line-per-node outline; pass detail=\"full\" with node_ids for \
                       the complete model of one subtree. Use this instead of a screenshot."
    )]
    async fn read_scene(
        &self,
        Parameters(args): Parameters<ReadSceneArgs>,
    ) -> Result<String, ErrorData> {
        let detail = args.detail.unwrap_or_default();
        let depth = args.depth.unwrap_or(12);
        // getCSSAsync roughly triples the payload and the outline never shows
        // per-node CSS, so it is ignored there rather than silently paid for.
        let include_css = args.include_css.unwrap_or(false) && detail == Detail::Full;

        let nodes = self.scene(args.node_ids, depth, include_css).await?;
        if nodes.is_empty() {
            return Ok("Nothing to read. Select a frame in Figma, or pass node_ids.".into());
        }

        match detail {
            Detail::Outline => Ok(outline::render(
                &nodes,
                &Budget {
                    max_nodes: args.max_nodes.unwrap_or(300) as usize,
                    max_depth: depth as usize,
                },
            )),
            Detail::Full => {
                let total: usize = nodes.iter().map(|n| n.count()).sum();
                // Warn rather than silently burn someone's context window.
                let note = if total > 120 {
                    format!(
                        "Note: {total} nodes. detail=\"outline\", a narrower node_ids, \
                         or a smaller depth costs far less.\n\n"
                    )
                } else {
                    String::new()
                };
                Ok(format!("{note}{}", pretty(&nodes)?))
            }
        }
    }

    /// Figma's own CSS, not our guess at it.
    #[tool(
        description = "Get Figma's own getCSSAsync() output for nodes — the exact CSS Figma itself \
                       would show in Dev Mode, for paint, typography and effects. Authoritative \
                       for appearance; it does not describe how a node relates to its parent, so \
                       pair it with read_scene for layout."
    )]
    async fn get_css(&self, Parameters(args): Parameters<NodesArgs>) -> Result<String, ErrorData> {
        let params = self.nodes_param(args.node_ids).await;
        let v = self.bridge.call("css", params).await?;
        pretty(&v)
    }

    /// Variables with their names — the thing REST gates behind Enterprise.
    #[tool(
        description = "List Figma variables as design tokens: full name, CSS custom-property name, \
                       type, collection, and the value for every mode. Also returns a ready-to-use \
                       tokens.css. The Plugin API exposes these on any plan, unlike the REST \
                       variables endpoints."
    )]
    async fn get_tokens(&self) -> Result<String, ErrorData> {
        let tokens = self.tokens().await?;
        if tokens.is_empty() {
            return Ok(
                "This file defines no variables. Colours and spacing will be emitted as \
                       literal values."
                    .into(),
            );
        }
        let css = codegen::generate(
            &ir::Node {
                id: "tokens".into(),
                name: "Tokens".into(),
                kind: "FRAME".into(),
                visible: true,
                width: 0.0,
                height: 0.0,
                layout: ir::Layout::Absolute,
                placement: ir::Placement::InFlow {
                    width: ir::Sizing::Hug,
                    height: ir::Sizing::Hug,
                    align_self: None,
                },
                padding: Default::default(),
                style: Default::default(),
                text: None,
                instance: None,
                figma_css: None,
                tokens: Default::default(),
                exportable: false,
                interactions: vec![],
                children: vec![],
            },
            &tokens,
            StyleMode::CssModules,
            None,
        );
        let sheet = css
            .files
            .iter()
            .find(|f| f.path == "tokens.css")
            .map(|f| f.contents.clone())
            .unwrap_or_default();

        Ok(format!(
            "{}\n\n--- tokens.css ---\n{}",
            pretty(&tokens)?,
            sheet
        ))
    }

    /// Component identity, so generated JSX references components.
    #[tool(
        description = "Inventory the component instances in a selection: which component or \
                       component set each instance came from, its variant property values, and \
                       whether it is from a shared library. This is what lets generated code emit \
                       <Button variant=\"primary\"> instead of a stack of divs."
    )]
    async fn get_components(
        &self,
        Parameters(args): Parameters<NodesArgs>,
    ) -> Result<String, ErrorData> {
        let params = self.nodes_param(args.node_ids).await;
        let v = self.bridge.call("components", params).await?;
        pretty(&v)
    }

    /// Assets at any scale, written to disk.
    #[tool(
        description = "Export nodes as real asset files (SVG, PNG, JPG, PDF) at any scale and write \
                       them to disk. Use for vectors, logos and images, which CSS cannot honestly \
                       reproduce."
    )]
    async fn export_assets(
        &self,
        Parameters(args): Parameters<ExportArgs>,
    ) -> Result<String, ErrorData> {
        let mut params = self.nodes_param(args.node_ids).await;
        params["format"] = json!(args.format.unwrap_or_else(|| "SVG".into()).to_uppercase());
        params["scale"] = json!(args.scale.unwrap_or(2.0));

        let v = self.bridge.call("export", params).await?;

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Exported {
            name: String,
            format: String,
            #[serde(default)]
            bytes_base64: String,
        }
        let items: Vec<Exported> = serde_json::from_value(v)
            .map_err(|e| bad(format!("plugin sent an export we could not decode: {e}")))?;

        let dir = args
            .out_dir
            .map(PathBuf::from)
            .unwrap_or_else(|| self.out_dir.join("assets"));
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| bad(format!("could not create {}: {e}", dir.display())))?;

        let mut written = Vec::new();
        for it in items {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(it.bytes_base64.as_bytes())
                .map_err(|e| bad(format!("bad base64 for {}: {e}", it.name)))?;
            let ext = it.format.to_lowercase();
            let path = dir.join(format!("{}.{ext}", codegen::kebab_case(&it.name)));
            tokio::fs::write(&path, &bytes)
                .await
                .map_err(|e| bad(format!("could not write {}: {e}", path.display())))?;
            written.push(json!({
                "file": path.display().to_string(),
                "bytes": bytes.len(),
            }));
        }

        if written.is_empty() {
            return Ok("Nothing exported. Select a node in Figma, or pass node_ids.".into());
        }
        pretty(&json!({ "written": written }))
    }

    /// Motion and flow: what the prototype actually does.
    #[tool(
        description = "Read the prototype interactions on a selection: triggers (click, hover, \
                       press, timeout), actions (navigate, open overlay, swap variant, set \
                       variable, open URL), and the motion for each — transition type, duration \
                       in ms, and a CSS-ready easing. Spring easings are simulated into a CSS \
                       linear() stop list rather than approximated, and the raw spring parameters \
                       are returned for motion libraries. Also summarises the screen-to-screen \
                       flow, which is the prototype's routing."
    )]
    async fn get_interactions(
        &self,
        Parameters(args): Parameters<NodesArgs>,
    ) -> Result<String, ErrorData> {
        let params = self.nodes_param(args.node_ids).await;
        let v = self.bridge.call("interactions", params).await?;

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Found {
            node_id: String,
            node_name: String,
            node_type: String,
            #[serde(default)]
            path: String,
            #[serde(default)]
            reactions: Vec<crate::raw::RawReaction>,
        }

        let found: Vec<Found> = serde_json::from_value(v)
            .map_err(|e| bad(format!("plugin sent interactions we could not decode: {e}")))?;

        if found.is_empty() {
            return Ok(
                "No prototype interactions on this selection. Wired connections and \
                       hover states live in Figma's Prototype tab."
                    .into(),
            );
        }

        let mut nodes = Vec::new();
        let mut flow = Vec::new();
        let mut approximations = Vec::new();

        for f in &found {
            let interactions: Vec<Interaction> = motion::read_reactions(&f.reactions);

            for i in &interactions {
                // The screen graph is the part that becomes routing.
                if let Action::Navigate {
                    destination_name, ..
                } = &i.action
                {
                    flow.push(format!(
                        "{} --{}--> {}",
                        f.node_name,
                        i.trigger_kind.to_lowercase().replace('_', " "),
                        destination_name.as_deref().unwrap_or("?")
                    ));
                }
                if let Some(t) = &i.transition
                    && t.easing_approximate
                {
                    approximations.push(format!(
                        "{}: {} has no exact CSS equivalent",
                        f.node_name, t.easing_kind
                    ));
                }
            }

            nodes.push(json!({
                "nodeId": f.node_id,
                "nodeName": f.node_name,
                "nodeType": f.node_type,
                "path": f.path,
                "interactions": interactions,
            }));
        }

        let mut out = format!("{} node(s) carry prototype interactions.\n", found.len());
        if !flow.is_empty() {
            out.push_str("\nScreen flow:\n");
            for f in &flow {
                out.push_str(&format!("  {f}\n"));
            }
        }
        if !approximations.is_empty() {
            out.push_str("\nEasings that are approximated, not exact:\n");
            for a in &approximations {
                out.push_str(&format!("  {a}\n"));
            }
        }
        out.push_str(&format!("\n{}", pretty(&json!({ "nodes": nodes }))?));
        Ok(out)
    }

    /// The payoff: canvas to React.
    #[tool(
        description = "Generate React + TypeScript from the canvas. Auto-layout becomes real \
                       flexbox (FILL becomes flex:1, HUG becomes fit-content), variables become \
                       CSS custom properties, component instances become component references \
                       with their variant props, and vectors are listed for export. Styles can be \
                       CSS modules, Tailwind or inline."
    )]
    async fn generate_code(
        &self,
        Parameters(args): Parameters<GenerateArgs>,
    ) -> Result<String, ErrorData> {
        let nodes = self
            .scene(args.node_ids, 24, args.include_css.unwrap_or(true))
            .await?;
        let Some(root) = nodes.first() else {
            return Ok("Nothing to generate. Select a frame in Figma, or pass node_ids.".into());
        };

        // Tokens are best-effort: a file with no variables still generates.
        let tokens = self.tokens().await.unwrap_or_default();

        let generated = codegen::generate(
            root,
            &tokens,
            args.style_mode.unwrap_or_default(),
            args.component_name.as_deref(),
        );

        let mut out = String::new();
        if args.write.unwrap_or(false) {
            let dir = self.out_dir.clone();
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| bad(format!("could not create {}: {e}", dir.display())))?;
            for f in &generated.files {
                let path = dir.join(&f.path);
                tokio::fs::write(&path, &f.contents)
                    .await
                    .map_err(|e| bad(format!("could not write {}: {e}", path.display())))?;
                out.push_str(&format!("wrote {}\n", path.display()));
            }
            out.push('\n');
        }

        out.push_str(&format!(
            "{} from {} nodes.\n",
            generated.component_name, generated.node_count
        ));
        if !generated.imported_components.is_empty() {
            out.push_str(&format!(
                "References existing components: {}. Wire these imports to your real paths.\n",
                generated.imported_components.join(", ")
            ));
        }
        if !generated.pending_assets.is_empty() {
            out.push_str(&format!(
                "{} vector node(s) need export_assets before this renders.\n",
                generated.pending_assets.len()
            ));
        }
        for w in &generated.warnings {
            out.push_str(&format!("warning: {w}\n"));
        }

        for f in &generated.files {
            out.push_str(&format!("\n--- {} ---\n{}", f.path, f.contents));
        }
        Ok(out)
    }
}

#[tool_handler]
impl ServerHandler for FigmaServer {
    fn get_info(&self) -> rmcp::model::ServerConfig {
        let mut info = rmcp::model::ServerConfig::default();
        info.protocol_version = rmcp::model::ProtocolVersion::LATEST;
        info.capabilities = rmcp::model::ServerCapabilities::builder()
            .enable_tools()
            .build();
        // from_build_env() resolves CARGO_PKG_* inside the rmcp crate, so it would
        // announce "rmcp" to the client. Report ourselves.
        let mut me = rmcp::model::Implementation::default();
        me.name = env!("CARGO_PKG_NAME").into();
        me.version = env!("CARGO_PKG_VERSION").into();
        info.server_info = me;
        info.instructions = Some(
            "Reads a live Figma canvas through a local plugin bridge and reconstructs it as \
                 code. Works with Figma Desktop and Figma in a browser tab.\n\n\
                 Start with figma_status to confirm the plugin is attached. Then get_selection to \
                 orient, read_scene for the structural model, and generate_code for React output. \
                 get_tokens returns design tokens with real names; export_assets writes vectors \
                 and images to disk.\n\n\
                 Layout comes from the scene model, not from screenshots or from getCSSAsync's \
                 fixed pixel sizes, so FILL and HUG survive as flex:1 and fit-content."
                .into(),
        );
        info
    }
}
