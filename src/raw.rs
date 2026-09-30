//! The payload shape the plugin sends us.
//!
//! Field names mirror the Figma Plugin API exactly (camelCase on the wire) so the
//! plugin side stays a thin projection with no renaming logic. All of it is
//! optional, because which properties exist depends on node type — a TEXT node has
//! no `layoutMode`, a GROUP has no `fills`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawPadding {
    #[serde(default)]
    pub top: f64,
    #[serde(default)]
    pub right: f64,
    #[serde(default)]
    pub bottom: f64,
    #[serde(default)]
    pub left: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawRadius {
    #[serde(default)]
    pub top_left: f64,
    #[serde(default)]
    pub top_right: f64,
    #[serde(default)]
    pub bottom_right: f64,
    #[serde(default)]
    pub bottom_left: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawConstraints {
    /// MIN | MAX | CENTER | STRETCH | SCALE
    #[serde(default)]
    pub horizontal: Option<String>,
    #[serde(default)]
    pub vertical: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawColor {
    #[serde(default)]
    pub r: f64,
    #[serde(default)]
    pub g: f64,
    #[serde(default)]
    pub b: f64,
    #[serde(default)]
    pub a: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawPaint {
    /// SOLID | GRADIENT_LINEAR | GRADIENT_RADIAL | IMAGE | VIDEO ...
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub visible: Option<bool>,
    #[serde(default)]
    pub opacity: Option<f64>,
    #[serde(default)]
    pub color: Option<RawColor>,
    /// Set when this paint is bound to a Figma variable: the token's full name.
    #[serde(default)]
    pub variable_name: Option<String>,
    #[serde(default)]
    pub variable_id: Option<String>,
    /// Gradient stops, already resolved to CSS-ready colours by the plugin.
    #[serde(default)]
    pub gradient_stops: Vec<RawGradientStop>,
    #[serde(default)]
    pub gradient_angle_deg: Option<f64>,
    /// IMAGE paints carry a hash we can export separately.
    #[serde(default)]
    pub image_hash: Option<String>,
    #[serde(default)]
    pub scale_mode: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawGradientStop {
    #[serde(default)]
    pub position: f64,
    #[serde(default)]
    pub color: Option<RawColor>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawEffect {
    /// DROP_SHADOW | INNER_SHADOW | LAYER_BLUR | BACKGROUND_BLUR
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub visible: Option<bool>,
    #[serde(default)]
    pub color: Option<RawColor>,
    #[serde(default)]
    pub offset_x: Option<f64>,
    #[serde(default)]
    pub offset_y: Option<f64>,
    #[serde(default)]
    pub radius: Option<f64>,
    #[serde(default)]
    pub spread: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawText {
    #[serde(default)]
    pub characters: String,
    #[serde(default)]
    pub font_family: Option<String>,
    #[serde(default)]
    pub font_style: Option<String>,
    #[serde(default)]
    pub font_size: Option<f64>,
    #[serde(default)]
    pub font_weight: Option<f64>,
    /// Already flattened to px or a percentage string by the plugin.
    #[serde(default)]
    pub line_height: Option<String>,
    #[serde(default)]
    pub letter_spacing: Option<String>,
    #[serde(default)]
    pub text_align_horizontal: Option<String>,
    #[serde(default)]
    pub text_align_vertical: Option<String>,
    #[serde(default)]
    pub text_decoration: Option<String>,
    #[serde(default)]
    pub text_case: Option<String>,
    /// Name of the bound text style, when one is applied.
    #[serde(default)]
    pub text_style_name: Option<String>,
}

/// An INSTANCE's identity: which component it came from, and its variant values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawInstance {
    #[serde(default)]
    pub component_id: Option<String>,
    #[serde(default)]
    pub component_name: Option<String>,
    /// Present when the main component belongs to a COMPONENT_SET.
    #[serde(default)]
    pub component_set_name: Option<String>,
    /// Variant and exposed properties, e.g. {"Size": "md", "State": "hover"}.
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
    #[serde(default)]
    pub is_remote: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawNode {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default)]
    pub absolute_bounding_box: Option<RawRect>,

    // --- auto layout, on the container ---
    /// NONE | HORIZONTAL | VERTICAL | GRID
    #[serde(default)]
    pub layout_mode: Option<String>,
    /// NO_WRAP | WRAP
    #[serde(default)]
    pub layout_wrap: Option<String>,
    #[serde(default)]
    pub item_spacing: Option<f64>,
    #[serde(default)]
    pub counter_axis_spacing: Option<f64>,
    /// MIN | CENTER | MAX | SPACE_BETWEEN
    #[serde(default)]
    pub primary_axis_align_items: Option<String>,
    /// MIN | CENTER | MAX | BASELINE
    #[serde(default)]
    pub counter_axis_align_items: Option<String>,
    #[serde(default)]
    pub padding: Option<RawPadding>,

    // --- how this node behaves inside its parent ---
    /// FIXED | HUG | FILL
    #[serde(default)]
    pub layout_sizing_horizontal: Option<String>,
    #[serde(default)]
    pub layout_sizing_vertical: Option<String>,
    /// AUTO | ABSOLUTE
    #[serde(default)]
    pub layout_positioning: Option<String>,
    #[serde(default)]
    pub layout_grow: Option<f64>,
    /// INHERIT | STRETCH — the per-child cross-axis override.
    #[serde(default)]
    pub layout_align: Option<String>,
    #[serde(default)]
    pub constraints: Option<RawConstraints>,
    /// Offset from the parent frame's origin, for absolutely placed children.
    #[serde(default)]
    pub relative_x: Option<f64>,
    #[serde(default)]
    pub relative_y: Option<f64>,

    // --- appearance ---
    #[serde(default)]
    pub opacity: Option<f64>,
    #[serde(default)]
    pub rotation: Option<f64>,
    #[serde(default)]
    pub clips_content: Option<bool>,
    #[serde(default)]
    pub corner_radius: Option<RawRadius>,
    #[serde(default)]
    pub fills: Vec<RawPaint>,
    #[serde(default)]
    pub strokes: Vec<RawPaint>,
    #[serde(default)]
    pub stroke_weight: Option<f64>,
    #[serde(default)]
    pub stroke_align: Option<String>,
    #[serde(default)]
    pub effects: Vec<RawEffect>,
    #[serde(default)]
    pub blend_mode: Option<String>,

    #[serde(default)]
    pub text: Option<RawText>,
    #[serde(default)]
    pub instance: Option<RawInstance>,

    /// Figma's own answer from `getCSSAsync()`. Ground truth, not our guess.
    #[serde(default)]
    pub css: Option<BTreeMap<String, String>>,

    /// Variable bindings by property, mapped to the token's full name.
    /// e.g. {"fills": "color/brand/primary", "itemSpacing": "space/md"}
    #[serde(default)]
    pub bound_variables: BTreeMap<String, String>,

    /// True when this node is marked as an export target in Figma.
    #[serde(default)]
    pub has_export_settings: bool,

    #[serde(default)]
    pub children: Vec<RawNode>,
}

fn default_true() -> bool {
    true
}

/// A Figma variable, resolved to a code-ready token.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawVariable {
    pub id: String,
    pub name: String,
    /// COLOR | FLOAT | STRING | BOOLEAN
    #[serde(default)]
    pub resolved_type: String,
    #[serde(default)]
    pub collection_name: Option<String>,
    /// Mode name -> CSS-ready value.
    #[serde(default)]
    pub values_by_mode: BTreeMap<String, String>,
    #[serde(default)]
    pub description: Option<String>,
}
