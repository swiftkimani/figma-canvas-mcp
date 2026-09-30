//! The MCP tool surface.
//!
//! Each tool maps to one capability the Plugin API gives us and the REST API does
//! not, or does worse. Tools return pretty JSON or source text, because that is
//! what a model actually reads.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex as AsyncMutex;

use base64::Engine as _;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::bridge::Bridge;
use crate::codegen::{self, StyleMode};
use crate::ir::{self, Token};
use crate::lsp::{self, LspClient};
use crate::motion::{self, Action, Interaction};
use crate::outline::{self, Budget};
use crate::raw::{RawNode, RawVariable};
use crate::stack::{self, Stack};

/// What this response cost, and what the other levels would have.
///
/// The numbers are measured, not guessed: the scene is already in hand, so the
/// other renderings can be produced and counted. Only `precise` and
/// `exhaustive` carry an estimate, because Figma's per-node CSS was not fetched
/// and is roughly twice the size of the model it annotates.
fn price_list(nodes: &[ir::Node], used: Fidelity, actual_bytes: usize) -> String {
    let tok = |b: usize| b / 4;

    let sketch = outline::render(nodes, &Fidelity::Sketch.budget(None, None)).len();
    let standard = outline::render(nodes, &Fidelity::Standard.budget(None, None)).len();
    let full = serde_json::to_string(nodes).map(|s| s.len()).unwrap_or(0);

    let mark = |f: Fidelity, n: usize, approx: bool| -> String {
        let here = if f == used { " ← this" } else { "" };
        format!(
            "{}{} {}{}",
            if approx { "≈" } else { "" },
            tok(n),
            match f {
                Fidelity::Sketch => "sketch",
                Fidelity::Standard => "standard",
                Fidelity::Precise => "precise",
                Fidelity::Exhaustive => "exhaustive",
            },
            here
        )
    };

    let total: usize = nodes.iter().map(|n| n.count()).sum();
    format!(
        "\n[{total} nodes · ~{} tokens spent · fidelity: {} · {} · {} · {}]",
        tok(actual_bytes),
        mark(Fidelity::Sketch, sketch, false),
        mark(Fidelity::Standard, standard, false),
        // Precise adds Figma's CSS, which was not fetched here.
        mark(Fidelity::Precise, full * 3, true),
        mark(Fidelity::Exhaustive, full * 3, true),
    )
}

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

/// How much detail to buy.
///
/// Each step costs materially more than the last, so the level is explicit and
/// every response prints what it spent and what the alternatives would have
/// cost. Nobody should discover the price after paying it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Fidelity {
    /// Shallow outline: the top few levels only. Answers "what is on this page".
    Sketch,
    /// Full-depth outline, repeated structures collapsed. The default, and
    /// enough to generate code from.
    #[default]
    Standard,
    /// The complete scene model as JSON, plus Figma's own CSS per node. Exact
    /// values for one subtree — not for a whole page.
    Precise,
    /// Everything `precise` has, with no depth or node limit at all.
    Exhaustive,
}

impl Fidelity {
    fn is_full(self) -> bool {
        matches!(self, Fidelity::Precise | Fidelity::Exhaustive)
    }

    fn budget(self, depth: Option<u32>, max_nodes: Option<u32>) -> Budget {
        let (d, n) = match self {
            Fidelity::Sketch => (3, 60),
            Fidelity::Standard => (12, 300),
            Fidelity::Precise => (12, 1_000),
            Fidelity::Exhaustive => (u32::MAX, u32::MAX),
        };
        Budget {
            max_depth: depth.unwrap_or(d) as usize,
            max_nodes: max_nodes.unwrap_or(n) as usize,
            // A sketch is the skeleton: structure without the words in it.
            show_content: self != Fidelity::Sketch,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadSceneArgs {
    /// Figma node ids to read. Omit to use the current selection in Figma.
    #[serde(default)]
    pub node_ids: Option<Vec<String>>,
    /// How much detail to buy: "sketch", "standard" (default), "precise" or
    /// "exhaustive". Each response states what it cost and what the others would.
    #[serde(default)]
    pub fidelity: Option<Fidelity>,
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
pub struct TokensArgs {
    /// Above this many tokens the JSON listing is summarised by collection, with
    /// the full values left to tokens.css. Default 80.
    #[serde(default)]
    pub max: Option<u32>,
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
    /// Force a styling approach. Omit to follow the project's own convention.
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

/// Lazily-started language server, shared across calls.
///
/// A TypeScript server can take tens of seconds to index a large project, so it
/// is started once and reused. A failure is remembered too — retrying a server
/// that is not installed on every call would just add latency to every call.
enum Lsp {
    Unchecked,
    Unavailable(String),
    Ready(Box<LspClient>),
}

#[derive(Clone)]
pub struct FigmaServer {
    bridge: Bridge,
    out_dir: PathBuf,
    /// The project whose conventions generated code should follow.
    project_root: PathBuf,
    lsp: Arc<AsyncMutex<Lsp>>,
}

#[tool_router]
impl FigmaServer {
    pub fn new(bridge: Bridge, out_dir: PathBuf, project_root: PathBuf) -> Self {
        Self {
            bridge,
            out_dir,
            project_root,
            lsp: Arc::new(AsyncMutex::new(Lsp::Unchecked)),
        }
    }

    /// Confirm each component exists and resolve where it actually lives.
    ///
    /// Returns a human-readable note for the caller. Any failure here is
    /// reported and stepped over: convention-derived imports are the fallback,
    /// and a missing language server must never block generating code.
    async fn verify_components(&self, detected: &mut Stack, wanted: &[String]) -> String {
        if wanted.is_empty() {
            return String::new();
        }

        let mut guard = self.lsp.lock().await;
        if matches!(*guard, Lsp::Unchecked) {
            *guard = match lsp::detect(&self.project_root, detected.framework) {
                None => Lsp::Unavailable(
                    "no language server found; import paths follow project convention".into(),
                ),
                Some(spec) => {
                    let reason = spec.reason.clone();
                    // Indexing dominates this; a short timeout would just fail.
                    match LspClient::spawn(&spec, &self.project_root, Duration::from_secs(60)).await
                    {
                        Ok(c) => {
                            tracing::info!("language server ready: {reason}");
                            Lsp::Ready(Box::new(c))
                        }
                        Err(e) => Lsp::Unavailable(format!("{reason} failed to start: {e}")),
                    }
                }
            };
        }

        let client = match &*guard {
            Lsp::Ready(c) => c,
            Lsp::Unavailable(why) => return format!("Imports not verified: {why}.\n"),
            Lsp::Unchecked => unreachable!("just initialised"),
        };

        let mut verified = Vec::new();
        let mut missing = Vec::new();
        let mut moved = Vec::new();

        for name in wanted {
            match client.find_component(name).await {
                Ok(Some(sym)) => {
                    if let Some(spec) = detected.import_specifier(&self.project_root, &sym.path) {
                        let conventional = detected.conventional_import_path(name);
                        if spec != conventional {
                            moved.push(format!("{name}: {conventional} -> {spec}"));
                        }
                        detected.verified_imports.insert(name.clone(), spec);
                        verified.push(name.clone());
                    }
                }
                Ok(None) => missing.push(name.clone()),
                Err(e) => {
                    return format!("Imports not verified: language server error: {e}.\n");
                }
            }
        }

        let mut note = String::new();
        if !verified.is_empty() {
            note.push_str(&format!(
                "Verified against the codebase: {}.\n",
                verified.join(", ")
            ));
        }
        for m in &moved {
            note.push_str(&format!("  corrected {m}\n"));
        }
        if !missing.is_empty() {
            // This is the useful signal: the design references something the
            // codebase does not have yet.
            note.push_str(&format!(
                "Not found in the codebase: {}. These imports are guesses \
                 — the component may need building first.\n",
                missing.join(", ")
            ));
        }
        note
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
                       token-named paint, text, and component/variant identity. fidelity is \
                       sketch | standard (default) | precise | exhaustive, each costing more \
                       than the last; every response reports what it spent and what the other \
                       levels would cost. Use this instead of a screenshot."
    )]
    async fn read_scene(
        &self,
        Parameters(args): Parameters<ReadSceneArgs>,
    ) -> Result<String, ErrorData> {
        let fidelity = args.fidelity.unwrap_or_default();
        let budget = fidelity.budget(args.depth, args.max_nodes);

        // getCSSAsync roughly triples the payload, and an outline never shows
        // per-node CSS, so it is only fetched where it is actually rendered.
        let include_css = args.include_css.unwrap_or(fidelity.is_full()) && fidelity.is_full();

        let nodes = self
            .scene(
                args.node_ids,
                budget.max_depth.min(u32::MAX as usize) as u32,
                include_css,
            )
            .await?;
        if nodes.is_empty() {
            return Ok("Nothing to read. Select a frame in Figma, or pass node_ids.".into());
        }

        let body = if fidelity.is_full() {
            pretty(&nodes)?
        } else {
            outline::render(&nodes, &budget)
        };

        Ok(format!(
            "{body}\n{}",
            price_list(&nodes, fidelity, body.len())
        ))
    }

    /// Figma's own CSS, not our guess at it.
    #[tool(
        description = "Get Figma's own getCSSAsync() output for nodes — the exact CSS Figma itself \
                       would show in Dev Mode, for paint, typography and effects. Authoritative \
                       for appearance; it does not describe how a node relates to its parent, so \
                       pair it with read_scene for layout."
    )]
    async fn get_css(&self, Parameters(args): Parameters<NodesArgs>) -> Result<String, ErrorData> {
        // Figma's CSS is verbose per node, so this is deliberately not a tool to
        // point at a whole page. Cap it and say so rather than flooding the caller.
        const MAX_CSS_NODES: usize = 40;
        let requested = args.node_ids.as_ref().map(|v| v.len()).unwrap_or(0);
        let (ids, trimmed) = match args.node_ids {
            Some(v) if v.len() > MAX_CSS_NODES => {
                let kept = v[..MAX_CSS_NODES].to_vec();
                (Some(kept), requested - MAX_CSS_NODES)
            }
            other => (other, 0),
        };

        let params = self.nodes_param(ids).await;
        let v = self.bridge.call("css", params).await?;
        let body = pretty(&v)?;
        if trimmed > 0 {
            return Ok(format!(
                "Returned the first {MAX_CSS_NODES} nodes; {trimmed} more were not \
                 requested from Figma. Call again with the remaining node_ids.\n\n{body}"
            ));
        }
        Ok(body)
    }

    /// Variables with their names — the thing REST gates behind Enterprise.
    #[tool(
        description = "List Figma variables as design tokens: full name, CSS custom-property name, \
                       type, collection, and the value for every mode. Also returns a ready-to-use \
                       tokens.css. The Plugin API exposes these on any plan, unlike the REST \
                       variables endpoints."
    )]
    async fn get_tokens(
        &self,
        Parameters(args): Parameters<TokensArgs>,
    ) -> Result<String, ErrorData> {
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
            &Stack::default(),
            Some(StyleMode::CssModules),
            None,
        );
        let sheet = css
            .files
            .iter()
            .find(|f| f.path == "tokens.css")
            .map(|f| f.contents.clone())
            .unwrap_or_default();

        // A mature design system has hundreds of variables, and the stylesheet
        // already carries every value. Listing the JSON too doubles the cost for
        // nothing, so past a threshold we summarise and let the sheet speak.
        let limit = args.max.unwrap_or(80) as usize;
        if tokens.len() > limit {
            let mut by_collection: BTreeMap<String, Vec<&Token>> = BTreeMap::new();
            for t in &tokens {
                by_collection
                    .entry(t.collection.clone().unwrap_or_else(|| "(none)".into()))
                    .or_default()
                    .push(t);
            }
            let mut out = format!(
                "{} tokens across {} collection(s). Listing summarised to keep the \
                 response small; every value is in tokens.css below.\n\n",
                tokens.len(),
                by_collection.len()
            );
            for (name, items) in &by_collection {
                let names: Vec<&str> = items.iter().take(12).map(|t| t.name.as_str()).collect();
                out.push_str(&format!(
                    "{name}: {} token(s) — {}{}\n",
                    items.len(),
                    names.join(", "),
                    if items.len() > names.len() {
                        format!(", +{} more", items.len() - names.len())
                    } else {
                        String::new()
                    }
                ));
            }
            out.push_str(&format!("\n--- tokens.css ---\n{sheet}"));
            return Ok(out);
        }

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

        // Match the project this code is going to land in, rather than guessing.
        let mut detected = stack::detect(&self.project_root);
        // Then let a language server correct that guess where it can.
        let verification = self
            .verify_components(
                &mut detected,
                &root.component_names().into_iter().collect::<Vec<_>>(),
            )
            .await;
        let generated = codegen::generate(
            root,
            &tokens,
            &detected,
            args.style_mode,
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
            "{}\n{verification}{} from {} nodes.\n",
            generated.stack_summary, generated.component_name, generated.node_count
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

impl FigmaServer {
    /// Call `read_scene` without going through MCP, for integration tests.
    #[doc(hidden)]
    pub async fn read_scene_for_test(
        &self,
        fidelity: Option<Fidelity>,
    ) -> Result<String, ErrorData> {
        self.read_scene(Parameters(ReadSceneArgs {
            node_ids: None,
            fidelity,
            depth: None,
            max_nodes: None,
            include_css: None,
        }))
        .await
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
