//! Accepting design data from whatever managed to read it.
//!
//! This exists because the reading half of this tool is the part that keeps
//! being unavailable. The plugin bridge needs the Figma Desktop app, which the
//! browser has no equivalent for. REST needs a token. Figma's own connector
//! needs edit access. A person can easily have none of those and still be
//! perfectly able to see the file in front of them.
//!
//! Everything *after* reading — the IR, the auto-layout mapping, repetition
//! collapsing, token resolution, stack detection, the health report, codegen —
//! works on any source. Locking it behind a privileged reader was the mistake.
//! So: hand it the JSON, however you got it.
//!
//! Two shapes are accepted, and told apart rather than guessed at:
//!
//! * **Figma REST** — what `/v1/files/:key/nodes` returns, including the
//!   wrapper with its `components` index. Any client that can make an HTTP
//!   request, or paste a response, can produce this.
//! * **Plugin projection** — what `plugin/code.js` sends over the bridge.

use anyhow::{Result, bail};
use serde_json::Value;

use crate::raw::RawNode;
use crate::rest::{self, ComponentIndex};

/// Which shape the caller handed us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// `{"nodes": {"1:2": {"document": {...}, "components": {...}}}}`
    RestNodesResponse,
    /// `{"document": {...}}` — one entry from the above.
    RestEntry,
    /// A bare REST document node, or an array of them.
    RestNode,
    /// The projection `plugin/code.js` produces.
    PluginProjection,
}

impl Shape {
    pub fn label(self) -> &'static str {
        match self {
            Shape::RestNodesResponse => "Figma REST /nodes response",
            Shape::RestEntry => "Figma REST node entry",
            Shape::RestNode => "Figma REST document node",
            Shape::PluginProjection => "plugin projection",
        }
    }
}

/// Tell the two shapes apart on evidence, not on a guess.
///
/// The decisive difference is text: REST puts `characters` on the node itself
/// and its typography in a sibling `style` object, while the plugin nests both
/// under `text`. Anything with a `nodes` or `document` wrapper is REST outright.
fn classify(v: &Value) -> Shape {
    if v.get("nodes").and_then(|n| n.as_object()).is_some() {
        return Shape::RestNodesResponse;
    }
    if v.get("document").is_some() {
        return Shape::RestEntry;
    }

    let first = match v {
        Value::Array(items) => items.first().unwrap_or(&Value::Null),
        other => other,
    };
    if has_plugin_text(first) {
        Shape::PluginProjection
    } else if has_rest_text(first) {
        Shape::RestNode
    } else {
        // No text anywhere to decide on. `reactions` and a nested `padding`
        // object only exist in the plugin projection.
        if first.get("reactions").is_some()
            || first.get("padding").map(|p| p.is_object()).unwrap_or(false)
        {
            Shape::PluginProjection
        } else {
            Shape::RestNode
        }
    }
}

fn has_plugin_text(v: &Value) -> bool {
    if v.get("text").map(|t| t.is_object()).unwrap_or(false) {
        return true;
    }
    children(v).iter().any(|c| has_plugin_text(c))
}

fn has_rest_text(v: &Value) -> bool {
    if v.get("characters").is_some() {
        return true;
    }
    children(v).iter().any(|c| has_rest_text(c))
}

fn children(v: &Value) -> Vec<&Value> {
    v.get("children")
        .and_then(|c| c.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

/// What a parse produced, and how it was read.
#[derive(Debug)]
pub struct Ingested {
    pub nodes: Vec<RawNode>,
    pub shape: Shape,
}

/// Parse design data of either shape into the shared node model.
pub fn ingest(value: &Value) -> Result<Ingested> {
    let shape = classify(value);

    let nodes = match shape {
        Shape::RestNodesResponse => {
            let entries = value
                .get("nodes")
                .and_then(|n| n.as_object())
                .expect("classified as a nodes response");
            let mut out = Vec::new();
            for entry in entries.values() {
                // The component index travels per entry in Figma's response.
                let index = ComponentIndex::from_response(entry);
                if let Some(doc) = entry.get("document") {
                    out.push(rest::node_from_rest(doc, None, &index));
                }
            }
            out
        }
        Shape::RestEntry => {
            let index = ComponentIndex::from_response(value);
            let doc = value.get("document").expect("classified as an entry");
            vec![rest::node_from_rest(doc, None, &index)]
        }
        Shape::RestNode => {
            // A bare node carries no component index, so instances will have no
            // component name. Better than refusing the input.
            let index = ComponentIndex::from_response(value);
            match value {
                Value::Array(items) => items
                    .iter()
                    .map(|n| rest::node_from_rest(n, None, &index))
                    .collect(),
                one => vec![rest::node_from_rest(one, None, &index)],
            }
        }
        Shape::PluginProjection => match value {
            Value::Array(_) => serde_json::from_value::<Vec<RawNode>>(value.clone())?,
            one => vec![serde_json::from_value::<RawNode>(one.clone())?],
        },
    };

    if nodes.is_empty() {
        bail!("no nodes found in that payload");
    }
    // A node with no id is almost always the wrong object handed over by
    // mistake — a file response, say, rather than a node.
    if nodes.iter().all(|n| n.id.is_empty()) {
        bail!(
            "parsed as {} but no node had an id; this looks like the wrong part \
             of the response. Pass the `nodes` object, a node entry, or a document node.",
            shape.label()
        );
    }
    Ok(Ingested { nodes, shape })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rest_doc() -> Value {
        json!({
            "id": "1:1", "name": "Card", "type": "FRAME", "visible": true,
            "absoluteBoundingBox": { "x": 0.0, "y": 0.0, "width": 320.0, "height": 180.0 },
            "layoutMode": "VERTICAL", "itemSpacing": 16.0,
            "children": [{
                "id": "1:2", "name": "Title", "type": "TEXT", "visible": true,
                "characters": "Hello",
                "style": { "fontFamily": "Inter", "fontSize": 24.0 }
            }]
        })
    }

    fn plugin_doc() -> Value {
        json!({
            "id": "1:1", "name": "Card", "type": "FRAME", "visible": true,
            "absoluteBoundingBox": { "x": 0.0, "y": 0.0, "width": 320.0, "height": 180.0 },
            "layoutMode": "VERTICAL", "itemSpacing": 16.0,
            "children": [{
                "id": "1:2", "name": "Title", "type": "TEXT", "visible": true,
                "text": { "characters": "Hello", "fontFamily": "Inter", "fontSize": 24.0 }
            }]
        })
    }

    #[test]
    fn a_full_rest_nodes_response_is_recognised_and_keeps_component_names() {
        let payload = json!({
            "name": "Bits",
            "nodes": {
                "1:1": {
                    "document": {
                        "id": "1:1", "type": "FRAME", "name": "Row", "visible": true,
                        "children": [{
                            "id": "1:3", "type": "INSTANCE", "name": "Buy", "visible": true,
                            "componentId": "10:5"
                        }]
                    },
                    "components": {
                        "10:5": { "name": "Button/primary", "componentSetId": "10:1", "remote": true }
                    },
                    "componentSets": { "10:1": { "name": "Button" } }
                }
            }
        });

        let got = ingest(&payload).expect("should parse");
        assert_eq!(got.shape, Shape::RestNodesResponse);
        assert_eq!(got.nodes.len(), 1);

        // The component index is why instances are identifiable; losing it here
        // would quietly downgrade every instance to an anonymous frame.
        let inst = got.nodes[0].children[0]
            .instance
            .as_ref()
            .expect("instance");
        assert_eq!(inst.component_set_name.as_deref(), Some("Button"));
        assert!(inst.is_remote);
    }

    #[test]
    fn the_two_shapes_are_told_apart_by_how_they_carry_text() {
        assert_eq!(classify(&rest_doc()), Shape::RestNode);
        assert_eq!(classify(&plugin_doc()), Shape::PluginProjection);
        // And in array form.
        assert_eq!(classify(&json!([rest_doc()])), Shape::RestNode);
        assert_eq!(classify(&json!([plugin_doc()])), Shape::PluginProjection);
    }

    #[test]
    fn both_shapes_reach_the_same_scene() {
        for payload in [rest_doc(), plugin_doc()] {
            let got = ingest(&payload).expect("should parse");
            let scene = crate::ir::build(&got.nodes[0]);
            assert_eq!(scene.name, "Card");
            assert_eq!(scene.count(), 2);
            assert_eq!(
                scene.text_content(),
                vec!["Hello"],
                "text must survive whichever shape it arrived in"
            );
            match &scene.layout {
                crate::ir::Layout::Flex { direction, gap, .. } => {
                    assert_eq!(*direction, crate::ir::Direction::Column);
                    assert_eq!(*gap, 16.0);
                }
                other => panic!("expected flex, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_rest_entry_without_the_outer_wrapper_works() {
        let payload = json!({ "document": rest_doc() });
        let got = ingest(&payload).expect("should parse");
        assert_eq!(got.shape, Shape::RestEntry);
        assert_eq!(got.nodes[0].id, "1:1");
    }

    #[test]
    fn the_wrong_object_is_refused_with_a_usable_message() {
        // A file-level response, which people reach for by mistake.
        let payload = json!({ "name": "Bits", "lastModified": "2026-01-01", "version": "1" });
        let err = ingest(&payload).unwrap_err().to_string();
        assert!(err.contains("no node had an id"), "{err}");
        assert!(
            err.contains("document node"),
            "the message must say what to pass: {err}"
        );

        assert!(
            ingest(&json!([]))
                .unwrap_err()
                .to_string()
                .contains("no nodes")
        );
    }

    #[test]
    fn a_shape_with_no_text_at_all_still_converts() {
        let payload = json!({
            "id": "1:1", "type": "RECTANGLE", "name": "Block", "visible": true,
            "absoluteBoundingBox": { "x": 0.0, "y": 0.0, "width": 10.0, "height": 10.0 }
        });
        let got = ingest(&payload).expect("should parse");
        assert_eq!(got.nodes[0].id, "1:1");
    }
}
