//! Reading a file through Figma's REST API.
//!
//! The plugin bridge is the better source — no rate limits, `getCSSAsync()`,
//! variables on any plan — but importing a development plugin requires the Figma
//! **Desktop app**, which the browser simply does not offer. For anyone working
//! in a browser tab, the bridge is unreachable.
//!
//! The REST API is reachable from anywhere, and crucially it needs only **view**
//! access to the file: a personal access token grants whatever the person can
//! already open. That makes this the only route that works for a browser-only
//! user, or for a file someone else owns and shared read-only.
//!
//! What it cannot do is covered honestly in [`Capabilities`], because silently
//! returning less is how a tool becomes untrustworthy.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::raw::*;

const API: &str = "https://api.figma.com/v1";

/// What this source can and cannot provide, relative to the plugin bridge.
#[derive(Debug, Clone, Copy)]
pub struct Capabilities;

impl Capabilities {
    /// Stated plainly so the caller can decide, rather than discovering gaps.
    pub const NOTES: &'static str = "\
Read over the REST API. Compared with the plugin bridge:
  - no getCSSAsync(), so appearance is derived from the node's own properties
  - variable *names* need an Enterprise-only endpoint, so token-bound fills come
    through as literal values unless that endpoint is available
  - rate limited by Figma, unlike the local bridge
  + needs only view access, and no Desktop app or plugin";
}

pub struct RestClient {
    http: reqwest::Client,
    token: String,
}

/// A file's component and component-set names, keyed by id.
///
/// REST gives an instance a `componentId` and nothing else; the names live in a
/// file-level map, so instances cannot be identified without it.
#[derive(Default)]
pub struct ComponentIndex {
    components: HashMap<String, RestComponent>,
    sets: HashMap<String, RestComponentSet>,
}

impl ComponentIndex {
    /// Build from the `components` and `componentSets` maps of a REST response.
    ///
    /// Both are optional: without them instances still convert, they just carry
    /// no component name, which is worse output but not a failure.
    pub fn from_response(value: &serde_json::Value) -> Self {
        fn read<T: serde::de::DeserializeOwned + Default>(
            value: &serde_json::Value,
            key: &str,
        ) -> HashMap<String, T> {
            value
                .get(key)
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default()
        }
        Self {
            components: read(value, "components"),
            sets: read(value, "componentSets"),
        }
    }
}

// Figma sends camelCase. Without this, `componentSetId` never binds and every
// instance loses its component-set name — silently, since the field is optional.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct RestComponent {
    #[serde(default)]
    name: String,
    #[serde(default)]
    component_set_id: Option<String>,
    #[serde(default)]
    remote: bool,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct RestComponentSet {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct NodesResponse {
    #[serde(default)]
    nodes: HashMap<String, NodeEntry>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NodeEntry {
    document: serde_json::Value,
    #[serde(default)]
    components: HashMap<String, RestComponent>,
    #[serde(default)]
    component_sets: HashMap<String, RestComponentSet>,
}

impl RestClient {
    pub fn new(token: impl Into<String>) -> Result<Self> {
        let token = token.into();
        if token.trim().is_empty() {
            bail!("no Figma token supplied");
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(concat!("figma-canvas-mcp/", env!("CARGO_PKG_VERSION")))
                .build()
                .context("could not build the HTTP client")?,
            token,
        })
    }

    /// Read from the environment, which is where a secret belongs.
    pub fn from_env() -> Option<Self> {
        let token = std::env::var("FIGMA_TOKEN")
            .or_else(|_| std::env::var("FIGMA_ACCESS_TOKEN"))
            .ok()?;
        Self::new(token).ok()
    }

    async fn get(&self, path: &str) -> Result<serde_json::Value> {
        let url = format!("{API}{path}");
        let res = self
            .http
            .get(&url)
            .header("X-Figma-Token", &self.token)
            .send()
            .await
            .with_context(|| format!("request to {path} failed"))?;

        let status = res.status();
        let body = res.text().await.unwrap_or_default();

        if !status.is_success() {
            // Figma's errors are terse; translate the ones people actually hit.
            let hint = match status.as_u16() {
                403 => {
                    " — the token is invalid, expired, or lacks the \
                        file_content:read scope. Tokens are created at \
                        figma.com > Settings > Security > Personal access tokens."
                }
                404 => {
                    " — the file or node does not exist, or your account cannot \
                        see it. The token only reaches files already shared with you."
                }
                429 => {
                    " — Figma is rate limiting. Wait, then narrow the request to \
                        specific node ids."
                }
                _ => "",
            };
            bail!(
                "Figma API returned {status}{hint}\n{}",
                truncate(&body, 400)
            );
        }

        serde_json::from_str(&body).context("Figma returned a body we could not decode")
    }

    /// Fetch one or more nodes and project them into the shared IR input shape.
    ///
    /// `depth` bounds how far Figma walks, which keeps large pages affordable.
    pub async fn fetch_nodes(
        &self,
        file_key: &str,
        node_ids: &[String],
        depth: Option<u32>,
    ) -> Result<Vec<RawNode>> {
        if node_ids.is_empty() {
            bail!("REST reads need at least one node id; the API has no concept of a selection");
        }
        // Figma wants comma-separated ids in `1:2` form.
        let ids = node_ids
            .iter()
            .map(|i| i.replace('-', ":"))
            .collect::<Vec<_>>()
            .join(",");

        let mut path = format!("/files/{file_key}/nodes?ids={ids}");
        if let Some(d) = depth {
            path.push_str(&format!("&depth={d}"));
        }

        let value = self.get(&path).await?;
        let parsed: NodesResponse =
            serde_json::from_value(value).context("unexpected shape from /files/:key/nodes")?;

        if parsed.nodes.is_empty() {
            bail!(
                "Figma returned no nodes for {ids}{}. Check the node-id in the URL.",
                parsed
                    .name
                    .map(|n| format!(" in \"{n}\""))
                    .unwrap_or_default()
            );
        }

        let mut out = Vec::new();
        for entry in parsed.nodes.into_values() {
            let index = ComponentIndex {
                components: entry.components,
                sets: entry.component_sets,
            };
            out.push(node_from_rest(&entry.document, None, &index));
        }
        Ok(out)
    }

    /// Confirm the token works and say who it belongs to.
    pub async fn whoami(&self) -> Result<String> {
        let v = self.get("/me").await?;
        Ok(v.get("email")
            .or_else(|| v.get("handle"))
            .and_then(|s| s.as_str())
            .unwrap_or("unknown")
            .to_string())
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
    format!("{}…", &s[..end])
}

// ---------------------------------------------------------------------------
// REST document -> RawNode
// ---------------------------------------------------------------------------

fn f(v: &serde_json::Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| x.as_f64())
}
fn s(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string)
}

fn rect(v: &serde_json::Value) -> Option<RawRect> {
    let b = v.get("absoluteBoundingBox")?;
    Some(RawRect {
        x: f(b, "x").unwrap_or(0.0),
        y: f(b, "y").unwrap_or(0.0),
        width: f(b, "width").unwrap_or(0.0),
        height: f(b, "height").unwrap_or(0.0),
    })
}

fn color(v: &serde_json::Value) -> Option<RawColor> {
    let c = v.as_object()?;
    Some(RawColor {
        r: c.get("r")?.as_f64()?,
        g: c.get("g")?.as_f64()?,
        b: c.get("b")?.as_f64()?,
        // REST folds alpha into the colour; the bridge keeps it separate.
        a: c.get("a").and_then(|x| x.as_f64()),
    })
}

fn paints(v: &serde_json::Value, key: &str) -> Vec<RawPaint> {
    v.get(key)
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter()
                .map(|p| RawPaint {
                    kind: s(p, "type").unwrap_or_default(),
                    visible: p.get("visible").and_then(|x| x.as_bool()).or(Some(true)),
                    opacity: f(p, "opacity"),
                    color: p.get("color").and_then(color),
                    image_hash: s(p, "imageRef"),
                    scale_mode: s(p, "scaleMode"),
                    gradient_stops: p
                        .get("gradientStops")
                        .and_then(|g| g.as_array())
                        .map(|stops| {
                            stops
                                .iter()
                                .map(|st| RawGradientStop {
                                    position: f(st, "position").unwrap_or(0.0),
                                    color: st.get("color").and_then(color),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    // REST exposes gradientHandlePositions rather than an angle.
                    gradient_angle_deg: None,
                    ..Default::default()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn effects(v: &serde_json::Value) -> Vec<RawEffect> {
    v.get("effects")
        .and_then(|e| e.as_array())
        .map(|arr| {
            arr.iter()
                .map(|e| RawEffect {
                    kind: s(e, "type").unwrap_or_default(),
                    visible: e.get("visible").and_then(|x| x.as_bool()).or(Some(true)),
                    color: e.get("color").and_then(color),
                    offset_x: e.get("offset").and_then(|o| f(o, "x")),
                    offset_y: e.get("offset").and_then(|o| f(o, "y")),
                    radius: f(e, "radius"),
                    spread: f(e, "spread"),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn padding(v: &serde_json::Value) -> Option<RawPadding> {
    // REST keeps these flat; the bridge nests them.
    let any = ["paddingTop", "paddingRight", "paddingBottom", "paddingLeft"]
        .iter()
        .any(|k| v.get(k).is_some());
    any.then(|| RawPadding {
        top: f(v, "paddingTop").unwrap_or(0.0),
        right: f(v, "paddingRight").unwrap_or(0.0),
        bottom: f(v, "paddingBottom").unwrap_or(0.0),
        left: f(v, "paddingLeft").unwrap_or(0.0),
    })
}

fn radius(v: &serde_json::Value) -> Option<RawRadius> {
    // Either one value, or four in [tl, tr, br, bl] order.
    if let Some(arr) = v.get("rectangleCornerRadii").and_then(|r| r.as_array())
        && arr.len() == 4
    {
        let g = |i: usize| arr[i].as_f64().unwrap_or(0.0);
        return Some(RawRadius {
            top_left: g(0),
            top_right: g(1),
            bottom_right: g(2),
            bottom_left: g(3),
        });
    }
    f(v, "cornerRadius").map(|c| RawRadius {
        top_left: c,
        top_right: c,
        bottom_right: c,
        bottom_left: c,
    })
}

fn text(v: &serde_json::Value) -> Option<RawText> {
    let characters = s(v, "characters")?;
    let style = v.get("style");
    let g = |k: &str| style.and_then(|st| s(st, k));
    let gf = |k: &str| style.and_then(|st| f(st, k));

    Some(RawText {
        characters,
        font_family: g("fontFamily"),
        font_style: g("fontPostScriptName"),
        font_size: gf("fontSize"),
        font_weight: gf("fontWeight"),
        // REST gives pixels or a percentage; prefer the resolved pixel value.
        line_height: gf("lineHeightPx")
            .map(|p| format!("{p}px"))
            .or_else(|| gf("lineHeightPercent").map(|p| format!("{p}%"))),
        letter_spacing: gf("letterSpacing").map(|l| format!("{l}px")),
        text_align_horizontal: g("textAlignHorizontal"),
        text_align_vertical: g("textAlignVertical"),
        text_decoration: g("textDecoration"),
        text_case: g("textCase"),
        text_style_name: None,
    })
}

fn instance(v: &serde_json::Value, index: &ComponentIndex) -> Option<RawInstance> {
    let component_id = s(v, "componentId")?;
    let component = index.components.get(&component_id);
    let set_name = component
        .and_then(|c| c.component_set_id.as_ref())
        .and_then(|sid| index.sets.get(sid))
        .map(|s| s.name.clone());

    // REST exposes variant values as `componentProperties` when present.
    let properties = v
        .get("componentProperties")
        .and_then(|p| p.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, val)| {
                    let value = val.get("value")?;
                    let as_text = value
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| value.as_bool().map(|b| b.to_string()))?;
                    Some((k.clone(), as_text))
                })
                .collect()
        })
        .unwrap_or_default();

    Some(RawInstance {
        component_id: Some(component_id),
        component_name: component.map(|c| c.name.clone()),
        component_set_name: set_name,
        properties,
        is_remote: component.map(|c| c.remote).unwrap_or(false),
    })
}

/// Variable bindings, by property. REST gives ids; names need an endpoint most
/// plans cannot reach, so the id is carried and the resolver may fill it later.
fn bound_variables(v: &serde_json::Value) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Some(bv) = v.get("boundVariables").and_then(|b| b.as_object()) else {
        return out;
    };
    for (key, entry) in bv {
        let alias = entry.as_array().and_then(|a| a.first()).unwrap_or(entry);
        if let Some(id) = alias.get("id").and_then(|i| i.as_str()) {
            out.insert(key.clone(), id.to_string());
        }
    }
    out
}

/// Map one Figma REST document node onto the shared [`RawNode`] shape.
///
/// Public because the mapping — not the HTTP call — is the part worth reusing.
/// Anything able to obtain Figma's REST JSON, by any means, can feed the rest of
/// this pipeline through here.
pub fn node_from_rest(
    v: &serde_json::Value,
    parent_box: Option<&RawRect>,
    index: &ComponentIndex,
) -> RawNode {
    let bbox = rect(v);
    let kind = s(v, "type").unwrap_or_default();

    // REST reports absolute coordinates only, so a child's offset within its
    // parent has to be derived. The bridge gets this for free from node.x/y.
    let (rel_x, rel_y) = match (&bbox, parent_box) {
        (Some(b), Some(p)) => (Some(b.x - p.x), Some(b.y - p.y)),
        _ => (None, None),
    };

    let children = v
        .get("children")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .map(|c| node_from_rest(c, bbox.as_ref(), index))
                .collect()
        })
        .unwrap_or_default();

    RawNode {
        id: s(v, "id").unwrap_or_default(),
        name: s(v, "name").unwrap_or_default(),
        kind: kind.clone(),
        visible: v.get("visible").and_then(|x| x.as_bool()).unwrap_or(true),
        absolute_bounding_box: bbox,
        layout_mode: s(v, "layoutMode"),
        layout_wrap: s(v, "layoutWrap"),
        item_spacing: f(v, "itemSpacing"),
        counter_axis_spacing: f(v, "counterAxisSpacing"),
        primary_axis_align_items: s(v, "primaryAxisAlignItems"),
        counter_axis_align_items: s(v, "counterAxisAlignItems"),
        padding: padding(v),
        layout_sizing_horizontal: s(v, "layoutSizingHorizontal"),
        layout_sizing_vertical: s(v, "layoutSizingVertical"),
        layout_positioning: s(v, "layoutPositioning"),
        layout_grow: f(v, "layoutGrow"),
        layout_align: s(v, "layoutAlign"),
        constraints: v.get("constraints").map(|c| RawConstraints {
            horizontal: s(c, "horizontal"),
            vertical: s(c, "vertical"),
        }),
        relative_x: rel_x,
        relative_y: rel_y,
        opacity: f(v, "opacity"),
        rotation: f(v, "rotation"),
        clips_content: v.get("clipsContent").and_then(|x| x.as_bool()),
        corner_radius: radius(v),
        fills: paints(v, "fills"),
        strokes: paints(v, "strokes"),
        stroke_weight: f(v, "strokeWeight"),
        stroke_align: s(v, "strokeAlign"),
        effects: effects(v),
        blend_mode: s(v, "blendMode"),
        text: (kind == "TEXT").then(|| text(v)).flatten(),
        instance: (kind == "INSTANCE").then(|| instance(v, index)).flatten(),
        // getCSSAsync has no REST equivalent.
        css: None,
        bound_variables: bound_variables(v),
        has_export_settings: v
            .get("exportSettings")
            .and_then(|e| e.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        reactions: Vec::new(),
        children,
    }
}

/// Pull the file key and node id out of any Figma URL.
///
/// Accepts `/design/`, `/file/` and `/proto/` links, because people paste
/// whichever one they happen to be looking at.
pub fn parse_url(url: &str) -> Option<(String, Option<String>)> {
    let after_host = url.split("figma.com/").nth(1)?;
    let mut parts = after_host.split('/');
    let kind = parts.next()?;
    if !matches!(kind, "design" | "file" | "proto" | "board" | "slides") {
        return None;
    }
    let file_key = parts.next()?.split('?').next()?.to_string();
    if file_key.is_empty() {
        return None;
    }

    // node-id is a query parameter and uses `-` where the API wants `:`.
    let node_id = url
        .split_once("node-id=")
        .and_then(|(_, rest)| rest.split('&').next())
        .map(|id| id.replace("%3A", ":").replace('-', ":"))
        .filter(|id| !id.is_empty());

    Some((file_key, node_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn urls_of_every_shape_are_parsed() {
        let cases = [
            (
                "https://www.figma.com/design/sviP36SZBBLLAD7L4qeda1/Bits--Copy-?node-id=4442-1220&t=x",
                ("sviP36SZBBLLAD7L4qeda1", Some("4442:1220")),
            ),
            (
                "https://www.figma.com/proto/sviP36SZBBLLAD7L4qeda1/Bits?node-id=4442-1220&scaling=min-zoom",
                ("sviP36SZBBLLAD7L4qeda1", Some("4442:1220")),
            ),
            (
                "https://figma.com/file/abc123XYZ/Old-Style-Link",
                ("abc123XYZ", None),
            ),
        ];
        for (url, (key, node)) in cases {
            let (k, n) = parse_url(url).unwrap_or_else(|| panic!("failed on {url}"));
            assert_eq!(k, key, "file key from {url}");
            assert_eq!(n.as_deref(), node, "node id from {url}");
        }
        assert!(parse_url("https://example.com/design/x").is_none());
    }

    fn index_with_button() -> ComponentIndex {
        ComponentIndex {
            components: HashMap::from([(
                "10:5".to_string(),
                RestComponent {
                    name: "Button/primary".into(),
                    component_set_id: Some("10:1".into()),
                    remote: true,
                },
            )]),
            sets: HashMap::from([(
                "10:1".to_string(),
                RestComponentSet {
                    name: "Button".into(),
                },
            )]),
        }
    }

    #[test]
    fn a_rest_frame_maps_onto_the_same_shape_the_bridge_produces() {
        let doc = json!({
            "id": "1:1", "name": "Card", "type": "FRAME", "visible": true,
            "absoluteBoundingBox": { "x": 100.0, "y": 50.0, "width": 320.0, "height": 180.0 },
            "layoutMode": "VERTICAL",
            "itemSpacing": 16.0,
            "counterAxisAlignItems": "CENTER",
            "paddingTop": 24.0, "paddingRight": 24.0, "paddingBottom": 24.0, "paddingLeft": 24.0,
            "cornerRadius": 12.0,
            "fills": [{ "type": "SOLID", "opacity": 1.0,
                        "color": { "r": 1.0, "g": 1.0, "b": 1.0, "a": 1.0 } }],
            "children": [{
                "id": "1:2", "name": "Title", "type": "TEXT", "visible": true,
                "absoluteBoundingBox": { "x": 124.0, "y": 74.0, "width": 272.0, "height": 40.0 },
                "layoutSizingHorizontal": "FILL",
                "characters": "Wireless headphones",
                "style": { "fontFamily": "Inter", "fontSize": 32.0, "fontWeight": 700.0,
                           "lineHeightPx": 40.0, "textAlignHorizontal": "CENTER" }
            }, {
                "id": "1:3", "name": "Buy", "type": "INSTANCE", "visible": true,
                "absoluteBoundingBox": { "x": 124.0, "y": 130.0, "width": 120.0, "height": 40.0 },
                "componentId": "10:5",
                "componentProperties": { "Size": { "value": "md", "type": "VARIANT" } }
            }]
        });

        let node = node_from_rest(&doc, None, &index_with_button());

        assert_eq!(node.layout_mode.as_deref(), Some("VERTICAL"));
        assert_eq!(node.item_spacing, Some(16.0));
        assert_eq!(node.padding.as_ref().map(|p| p.top), Some(24.0));
        assert_eq!(node.corner_radius.as_ref().map(|r| r.top_left), Some(12.0));
        assert_eq!(node.children.len(), 2);

        // Absolute REST coordinates become parent-relative offsets.
        let title = &node.children[0];
        assert_eq!(title.relative_x, Some(24.0), "124 - 100");
        assert_eq!(title.relative_y, Some(24.0), "74 - 50");
        assert_eq!(title.layout_sizing_horizontal.as_deref(), Some("FILL"));

        let t = title.text.as_ref().expect("text");
        assert_eq!(t.characters, "Wireless headphones");
        assert_eq!(t.font_size, Some(32.0));
        assert_eq!(t.line_height.as_deref(), Some("40px"));

        // The component index is what makes an instance identifiable at all.
        let inst = node.children[1].instance.as_ref().expect("instance");
        assert_eq!(inst.component_set_name.as_deref(), Some("Button"));
        assert_eq!(inst.component_name.as_deref(), Some("Button/primary"));
        assert_eq!(inst.properties.get("Size").map(String::as_str), Some("md"));
        assert!(inst.is_remote);
    }

    #[test]
    fn the_whole_pipeline_accepts_a_rest_node() {
        // The point of mapping onto RawNode: everything downstream just works.
        let doc = json!({
            "id": "1:1", "name": "Row", "type": "FRAME", "visible": true,
            "absoluteBoundingBox": { "x": 0.0, "y": 0.0, "width": 280.0, "height": 48.0 },
            "layoutMode": "HORIZONTAL", "itemSpacing": 8.0,
            "primaryAxisAlignItems": "SPACE_BETWEEN",
            "children": [{
                "id": "1:2", "name": "Label", "type": "TEXT", "visible": true,
                "absoluteBoundingBox": { "x": 0.0, "y": 0.0, "width": 100.0, "height": 20.0 },
                "characters": "Wi-Fi",
                "style": { "fontFamily": "Inter", "fontSize": 14.0 }
            }]
        });
        let scene = crate::ir::build(&node_from_rest(&doc, None, &ComponentIndex::default()));

        match &scene.layout {
            crate::ir::Layout::Flex {
                direction,
                gap,
                justify,
                ..
            } => {
                assert_eq!(*direction, crate::ir::Direction::Row);
                assert_eq!(*gap, 8.0);
                assert_eq!(*justify, crate::ir::Justify::SpaceBetween);
            }
            other => panic!("expected flex, got {other:?}"),
        }

        let out = crate::codegen::generate(
            &scene,
            &[],
            &crate::stack::Stack::default(),
            Some(crate::codegen::StyleMode::CssModules),
            None,
        );
        assert!(out.files[0].contents.contains("Wi-Fi"));
    }

    #[test]
    fn the_component_index_deserialises_from_figmas_actual_json() {
        // Built from JSON rather than in Rust: constructing it by hand is what
        // let a missing rename_all go unnoticed.
        let response = json!({
            "components": {
                "10:5": { "name": "Button/primary", "componentSetId": "10:1", "remote": true }
            },
            "componentSets": { "10:1": { "name": "Button" } }
        });
        let index = ComponentIndex::from_response(&response);

        let doc = json!({
            "id": "1:1", "type": "INSTANCE", "name": "Buy", "visible": true,
            "componentId": "10:5"
        });
        let inst = node_from_rest(&doc, None, &index)
            .instance
            .expect("instance");
        assert_eq!(inst.component_name.as_deref(), Some("Button/primary"));
        assert_eq!(
            inst.component_set_name.as_deref(),
            Some("Button"),
            "componentSetId must bind from camelCase JSON"
        );
        assert!(inst.is_remote);
    }

    #[test]
    fn four_corner_radii_are_read_in_figmas_order() {
        let doc = json!({
            "id": "1:1", "type": "RECTANGLE", "visible": true,
            "rectangleCornerRadii": [1.0, 2.0, 3.0, 4.0]
        });
        let r = node_from_rest(&doc, None, &ComponentIndex::default())
            .corner_radius
            .expect("radii");
        assert_eq!(
            (r.top_left, r.top_right, r.bottom_right, r.bottom_left),
            (1.0, 2.0, 3.0, 4.0)
        );
    }

    #[test]
    fn an_empty_token_is_refused_rather_than_sent() {
        assert!(RestClient::new("   ").is_err());
        assert!(RestClient::new("figd_realish").is_ok());
    }
}
