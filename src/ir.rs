//! The normalized scene: a reconstruction of the canvas that is ready to become code.
//!
//! The mapping from Figma's layout model to CSS lives here rather than in the
//! plugin, so it is pure, testable Rust. That matters because this mapping is the
//! whole ballgame — get it wrong and you emit a pile of `position: absolute`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::raw::{RawInstance, RawNode, RawPaint, RawText, RawVariable};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Row,
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Justify {
    Start,
    Center,
    End,
    SpaceBetween,
}

impl Justify {
    pub fn css(self) -> &'static str {
        match self {
            Justify::Start => "flex-start",
            Justify::Center => "center",
            Justify::End => "flex-end",
            Justify::SpaceBetween => "space-between",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    Start,
    Center,
    End,
    Baseline,
    Stretch,
}

impl Align {
    pub fn css(self) -> &'static str {
        match self {
            Align::Start => "flex-start",
            Align::Center => "center",
            Align::End => "flex-end",
            Align::Baseline => "baseline",
            Align::Stretch => "stretch",
        }
    }
}

/// How a node sizes itself on one axis.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sizing {
    /// Figma FIXED — an explicit px value.
    Fixed(f64),
    /// Figma HUG — shrink to content.
    Hug,
    /// Figma FILL — take the remaining space on the parent's axis.
    Fill,
}

/// How this node lays out its own children.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Layout {
    /// No auto layout: children are absolutely positioned.
    Absolute,
    Flex {
        direction: Direction,
        gap: f64,
        /// Cross-axis gap, only meaningful when wrapping.
        cross_gap: Option<f64>,
        wrap: bool,
        justify: Justify,
        align: Align,
    },
    /// Figma's newer grid auto-layout.
    Grid { gap: f64 },
}

/// How this node is placed by its parent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Placement {
    /// In the parent's flex flow.
    InFlow {
        width: Sizing,
        height: Sizing,
        /// Figma's per-child STRETCH override on the cross axis.
        align_self: Option<Align>,
    },
    /// Escaped the flow, or the parent has no auto layout.
    Absolute {
        x: f64,
        y: f64,
        width: Sizing,
        height: Sizing,
        /// Figma constraints, which decide whether to pin left/right/top/bottom.
        anchor_h: Anchor,
        anchor_v: Anchor,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    Min,
    Max,
    Center,
    Stretch,
    Scale,
}

impl Anchor {
    fn parse(s: Option<&str>) -> Anchor {
        match s.unwrap_or("MIN") {
            "MAX" => Anchor::Max,
            "CENTER" => Anchor::Center,
            "STRETCH" => Anchor::Stretch,
            "SCALE" => Anchor::Scale,
            _ => Anchor::Min,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Edges {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl Edges {
    pub fn is_zero(&self) -> bool {
        self.top == 0.0 && self.right == 0.0 && self.bottom == 0.0 && self.left == 0.0
    }
    pub fn uniform(&self) -> Option<f64> {
        if self.top == self.right && self.right == self.bottom && self.bottom == self.left {
            Some(self.top)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Radius {
    pub tl: f64,
    pub tr: f64,
    pub br: f64,
    pub bl: f64,
}

impl Radius {
    pub fn is_zero(&self) -> bool {
        self.tl == 0.0 && self.tr == 0.0 && self.br == 0.0 && self.bl == 0.0
    }
    pub fn uniform(&self) -> Option<f64> {
        if self.tl == self.tr && self.tr == self.br && self.br == self.bl {
            Some(self.tl)
        } else {
            None
        }
    }
}

/// A colour or gradient, carrying its token name when one is bound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fill {
    /// CSS-ready value: `#0d99ff`, `rgb(13 153 255 / 0.5)`, or a `linear-gradient(...)`.
    pub value: String,
    /// The Figma variable name, if bound — e.g. `color/brand/primary`.
    pub token: Option<String>,
    /// `--color-brand-primary`, derived from `token`.
    pub css_var: Option<String>,
    pub kind: String,
    pub image_hash: Option<String>,
}

impl Fill {
    /// What to actually write into CSS: the token reference when we have one, so
    /// generated code inherits theming instead of hard-coding hex.
    pub fn css_value(&self) -> String {
        match (&self.css_var, &self.value) {
            (Some(var), v) if !v.is_empty() => format!("var({var}, {v})"),
            (Some(var), _) => format!("var({var})"),
            (None, v) => v.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shadow {
    pub inset: bool,
    pub x: f64,
    pub y: f64,
    pub blur: f64,
    pub spread: f64,
    pub color: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Style {
    pub fills: Vec<Fill>,
    pub strokes: Vec<Fill>,
    pub stroke_width: Option<f64>,
    pub radius: Radius,
    pub shadows: Vec<Shadow>,
    pub blur: Option<f64>,
    pub opacity: Option<f64>,
    pub rotation: Option<f64>,
    pub clip: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Text {
    pub content: String,
    pub font_family: Option<String>,
    pub font_size: Option<f64>,
    pub font_weight: Option<f64>,
    pub line_height: Option<String>,
    pub letter_spacing: Option<String>,
    pub align: Option<String>,
    pub transform: Option<String>,
    pub decoration: Option<String>,
    pub style_name: Option<String>,
}

/// An instance's component identity — what makes `<Button variant="primary">`
/// possible instead of a nest of divs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    /// The component set name when it has variants, else the component name.
    pub component: String,
    pub component_id: Option<String>,
    /// Variant and exposed props, already keyed for JSX.
    pub props: BTreeMap<String, String>,
    pub from_library: bool,
}

impl Instance {
    /// PascalCase JSX tag for this component.
    pub fn tag(&self) -> String {
        pascal_case(&self.component)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub name: String,
    /// Figma node type, verbatim: FRAME, TEXT, INSTANCE, RECTANGLE, VECTOR, ...
    pub kind: String,
    pub visible: bool,
    pub width: f64,
    pub height: f64,
    pub layout: Layout,
    pub placement: Placement,
    pub padding: Edges,
    pub style: Style,
    pub text: Option<Text>,
    pub instance: Option<Instance>,
    /// Figma's own `getCSSAsync()` output, when requested.
    pub figma_css: Option<BTreeMap<String, String>>,
    /// Token bindings by Figma property name.
    pub tokens: BTreeMap<String, String>,
    pub exportable: bool,
    pub children: Vec<Node>,
}

impl Node {
    /// Does this subtree need a wrapper element at all, or is it pure text?
    pub fn is_text_only(&self) -> bool {
        self.text.is_some() && self.children.is_empty()
    }

    /// A vector-ish node we should export as an asset rather than rebuild in CSS.
    pub fn is_vector(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "VECTOR" | "BOOLEAN_OPERATION" | "STAR" | "POLYGON" | "LINE"
        )
    }

    pub fn count(&self) -> usize {
        1 + self.children.iter().map(Node::count).sum::<usize>()
    }

    pub fn depth(&self) -> usize {
        1 + self.children.iter().map(Node::depth).max().unwrap_or(0)
    }
}

/// A design token lifted from a Figma variable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub id: String,
    /// `color/brand/primary`
    pub name: String,
    /// `--color-brand-primary`
    pub css_var: String,
    pub kind: String,
    pub collection: Option<String>,
    pub values_by_mode: BTreeMap<String, String>,
    pub description: Option<String>,
}

impl From<RawVariable> for Token {
    fn from(v: RawVariable) -> Self {
        Token {
            css_var: css_var_name(&v.name),
            id: v.id,
            name: v.name,
            kind: v.resolved_type,
            collection: v.collection_name,
            values_by_mode: v.values_by_mode,
            description: v.description,
        }
    }
}

/// Turn `color/brand/primary` into `--color-brand-primary`.
pub fn css_var_name(figma_name: &str) -> String {
    let mut out = String::from("--");
    let mut prev_dash = false;
    for ch in figma_name.chars() {
        let c = match ch {
            'a'..='z' | '0'..='9' => ch,
            'A'..='Z' => ch.to_ascii_lowercase(),
            _ => '-',
        };
        if c == '-' {
            if prev_dash {
                continue;
            }
            prev_dash = true;
        } else {
            prev_dash = false;
        }
        out.push(c);
    }
    out.trim_end_matches('-').to_string()
}

/// `primary button / large` -> `PrimaryButtonLarge`
pub fn pascal_case(s: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            if upper {
                out.extend(ch.to_uppercase());
                upper = false;
            } else {
                out.push(ch);
            }
        } else {
            upper = true;
        }
    }
    if out.is_empty() {
        return "Component".into();
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'C');
    }
    out
}

// ---------------------------------------------------------------------------
// raw -> IR
// ---------------------------------------------------------------------------

/// Context a child needs from its parent to decide its own placement.
#[derive(Debug, Clone, Copy)]
struct ParentCtx {
    /// Parent has auto layout, so in-flow children exist.
    flow: Option<Direction>,
}

pub fn build(raw: &RawNode) -> Node {
    convert(raw, ParentCtx { flow: None })
}

fn convert(raw: &RawNode, parent: ParentCtx) -> Node {
    let layout = read_layout(raw);
    let child_ctx = ParentCtx {
        flow: match &layout {
            Layout::Flex { direction, .. } => Some(*direction),
            _ => None,
        },
    };

    let bbox = raw.absolute_bounding_box.clone().unwrap_or_default();

    Node {
        id: raw.id.clone(),
        name: raw.name.clone(),
        kind: raw.kind.clone(),
        visible: raw.visible,
        width: bbox.width,
        height: bbox.height,
        placement: read_placement(raw, parent),
        layout,
        padding: raw
            .padding
            .as_ref()
            .map(|p| Edges {
                top: p.top,
                right: p.right,
                bottom: p.bottom,
                left: p.left,
            })
            .unwrap_or_default(),
        style: read_style(raw),
        text: raw.text.as_ref().map(read_text),
        instance: raw.instance.as_ref().and_then(read_instance),
        figma_css: raw.css.clone(),
        tokens: raw.bound_variables.clone(),
        exportable: raw.has_export_settings,
        children: raw
            .children
            .iter()
            .filter(|c| c.visible)
            .map(|c| convert(c, child_ctx))
            .collect(),
    }
}

fn read_layout(raw: &RawNode) -> Layout {
    match raw.layout_mode.as_deref() {
        Some("HORIZONTAL") | Some("VERTICAL") => {
            let direction = if raw.layout_mode.as_deref() == Some("HORIZONTAL") {
                Direction::Row
            } else {
                Direction::Column
            };
            let wrap = raw.layout_wrap.as_deref() == Some("WRAP");
            Layout::Flex {
                direction,
                gap: raw.item_spacing.unwrap_or(0.0),
                cross_gap: if wrap { raw.counter_axis_spacing } else { None },
                wrap,
                justify: match raw.primary_axis_align_items.as_deref() {
                    Some("CENTER") => Justify::Center,
                    Some("MAX") => Justify::End,
                    Some("SPACE_BETWEEN") => Justify::SpaceBetween,
                    _ => Justify::Start,
                },
                align: match raw.counter_axis_align_items.as_deref() {
                    Some("CENTER") => Align::Center,
                    Some("MAX") => Align::End,
                    Some("BASELINE") => Align::Baseline,
                    _ => Align::Start,
                },
            }
        }
        Some("GRID") => Layout::Grid {
            gap: raw.item_spacing.unwrap_or(0.0),
        },
        _ => Layout::Absolute,
    }
}

fn read_sizing(mode: Option<&str>, explicit: f64) -> Sizing {
    match mode {
        Some("HUG") => Sizing::Hug,
        Some("FILL") => Sizing::Fill,
        _ => Sizing::Fixed(explicit),
    }
}

fn read_placement(raw: &RawNode, parent: ParentCtx) -> Placement {
    let bbox = raw.absolute_bounding_box.clone().unwrap_or_default();
    let width = read_sizing(raw.layout_sizing_horizontal.as_deref(), bbox.width);
    let height = read_sizing(raw.layout_sizing_vertical.as_deref(), bbox.height);

    let escaped = raw.layout_positioning.as_deref() == Some("ABSOLUTE");
    if parent.flow.is_none() || escaped {
        let c = raw.constraints.clone().unwrap_or_default();
        return Placement::Absolute {
            x: raw.relative_x.unwrap_or(bbox.x),
            y: raw.relative_y.unwrap_or(bbox.y),
            width,
            height,
            anchor_h: Anchor::parse(c.horizontal.as_deref()),
            anchor_v: Anchor::parse(c.vertical.as_deref()),
        };
    }

    // In flow. Figma expresses cross-axis stretch per child via layoutAlign.
    let align_self = match raw.layout_align.as_deref() {
        Some("STRETCH") => Some(Align::Stretch),
        _ => None,
    };
    Placement::InFlow {
        width,
        height,
        align_self,
    }
}

fn read_style(raw: &RawNode) -> Style {
    Style {
        fills: raw.fills.iter().filter_map(read_fill).collect(),
        strokes: raw.strokes.iter().filter_map(read_fill).collect(),
        stroke_width: raw.stroke_weight.filter(|w| *w > 0.0),
        radius: raw
            .corner_radius
            .as_ref()
            .map(|r| Radius {
                tl: r.top_left,
                tr: r.top_right,
                br: r.bottom_right,
                bl: r.bottom_left,
            })
            .unwrap_or_default(),
        shadows: raw
            .effects
            .iter()
            .filter(|e| e.visible.unwrap_or(true))
            .filter_map(|e| match e.kind.as_str() {
                "DROP_SHADOW" | "INNER_SHADOW" => Some(Shadow {
                    inset: e.kind == "INNER_SHADOW",
                    x: e.offset_x.unwrap_or(0.0),
                    y: e.offset_y.unwrap_or(0.0),
                    blur: e.radius.unwrap_or(0.0),
                    spread: e.spread.unwrap_or(0.0),
                    color: e
                        .color
                        .as_ref()
                        .map(css_color)
                        .unwrap_or_else(|| "rgb(0 0 0 / 0.25)".into()),
                }),
                _ => None,
            })
            .collect(),
        blur: raw
            .effects
            .iter()
            .filter(|e| e.visible.unwrap_or(true) && e.kind == "LAYER_BLUR")
            .filter_map(|e| e.radius)
            .next(),
        opacity: raw.opacity.filter(|o| *o < 1.0),
        rotation: raw.rotation.filter(|r| r.abs() > f64::EPSILON),
        clip: raw.clips_content.unwrap_or(false),
    }
}

fn read_fill(p: &RawPaint) -> Option<Fill> {
    if !p.visible.unwrap_or(true) {
        return None;
    }
    let value = match p.kind.as_str() {
        "SOLID" => p.color.as_ref().map(|c| {
            let mut c = c.clone();
            // Paint-level opacity multiplies the colour's own alpha.
            if let Some(o) = p.opacity {
                c.a = Some(c.a.unwrap_or(1.0) * o);
            }
            css_color(&c)
        })?,
        k if k.starts_with("GRADIENT") => {
            let stops: Vec<String> = p
                .gradient_stops
                .iter()
                .filter_map(|s| {
                    s.color
                        .as_ref()
                        .map(|c| format!("{} {:.1}%", css_color(c), s.position * 100.0))
                })
                .collect();
            if stops.is_empty() {
                return None;
            }
            if k == "GRADIENT_RADIAL" {
                format!("radial-gradient({})", stops.join(", "))
            } else {
                format!(
                    "linear-gradient({}deg, {})",
                    p.gradient_angle_deg.unwrap_or(180.0).round(),
                    stops.join(", ")
                )
            }
        }
        "IMAGE" => String::new(),
        _ => return None,
    };

    Some(Fill {
        value,
        css_var: p.variable_name.as_deref().map(css_var_name),
        token: p.variable_name.clone(),
        kind: p.kind.clone(),
        image_hash: p.image_hash.clone(),
    })
}

fn css_color(c: &crate::raw::RawColor) -> String {
    let r = (c.r * 255.0).round().clamp(0.0, 255.0) as u8;
    let g = (c.g * 255.0).round().clamp(0.0, 255.0) as u8;
    let b = (c.b * 255.0).round().clamp(0.0, 255.0) as u8;
    match c.a {
        Some(a) if a < 0.999 => format!("rgb({r} {g} {b} / {:.3})", a),
        _ => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

fn read_text(t: &RawText) -> Text {
    Text {
        content: t.characters.clone(),
        font_family: t.font_family.clone(),
        font_size: t.font_size,
        font_weight: t.font_weight,
        line_height: t.line_height.clone(),
        letter_spacing: t.letter_spacing.clone(),
        align: t.text_align_horizontal.as_ref().map(|a| match a.as_str() {
            "CENTER" => "center".into(),
            "RIGHT" => "right".into(),
            "JUSTIFIED" => "justify".into(),
            _ => "left".to_string(),
        }),
        transform: t.text_case.as_ref().and_then(|c| match c.as_str() {
            "UPPER" => Some("uppercase".into()),
            "LOWER" => Some("lowercase".into()),
            "TITLE" => Some("capitalize".into()),
            _ => None,
        }),
        decoration: t.text_decoration.as_ref().and_then(|d| match d.as_str() {
            "UNDERLINE" => Some("underline".into()),
            "STRIKETHROUGH" => Some("line-through".into()),
            _ => None,
        }),
        style_name: t.text_style_name.clone(),
    }
}

fn read_instance(i: &RawInstance) -> Option<Instance> {
    let component = i
        .component_set_name
        .clone()
        .or_else(|| i.component_name.clone())?;
    Some(Instance {
        component,
        component_id: i.component_id.clone(),
        props: i.properties.clone(),
        from_library: i.is_remote,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_var_names_are_sanitized() {
        assert_eq!(css_var_name("color/brand/primary"), "--color-brand-primary");
        assert_eq!(css_var_name("Space / MD"), "--space-md");
        assert_eq!(css_var_name("radius.lg"), "--radius-lg");
    }

    #[test]
    fn pascal_case_handles_figma_names() {
        assert_eq!(pascal_case("primary button / large"), "PrimaryButtonLarge");
        assert_eq!(pascal_case("2-col grid"), "C2ColGrid");
        assert_eq!(pascal_case("???"), "Component");
    }

    #[test]
    fn horizontal_auto_layout_becomes_row_flex() {
        let raw = RawNode {
            id: "1:2".into(),
            kind: "FRAME".into(),
            layout_mode: Some("HORIZONTAL".into()),
            item_spacing: Some(12.0),
            primary_axis_align_items: Some("SPACE_BETWEEN".into()),
            counter_axis_align_items: Some("CENTER".into()),
            visible: true,
            ..Default::default()
        };
        match build(&raw).layout {
            Layout::Flex {
                direction,
                gap,
                justify,
                align,
                wrap,
                ..
            } => {
                assert_eq!(direction, Direction::Row);
                assert_eq!(gap, 12.0);
                assert_eq!(justify, Justify::SpaceBetween);
                assert_eq!(align, Align::Center);
                assert!(!wrap);
            }
            other => panic!("expected flex, got {other:?}"),
        }
    }

    #[test]
    fn child_of_autolayout_parent_is_in_flow_and_fill_is_preserved() {
        let child = RawNode {
            id: "1:3".into(),
            kind: "FRAME".into(),
            layout_sizing_horizontal: Some("FILL".into()),
            layout_sizing_vertical: Some("HUG".into()),
            visible: true,
            ..Default::default()
        };
        let parent = RawNode {
            id: "1:2".into(),
            kind: "FRAME".into(),
            layout_mode: Some("VERTICAL".into()),
            visible: true,
            children: vec![child],
            ..Default::default()
        };
        let node = build(&parent);
        match &node.children[0].placement {
            Placement::InFlow { width, height, .. } => {
                assert_eq!(*width, Sizing::Fill);
                assert_eq!(*height, Sizing::Hug);
            }
            other => panic!("expected in-flow, got {other:?}"),
        }
    }

    #[test]
    fn absolute_positioning_escapes_the_flow() {
        let child = RawNode {
            id: "1:3".into(),
            kind: "FRAME".into(),
            layout_positioning: Some("ABSOLUTE".into()),
            relative_x: Some(8.0),
            relative_y: Some(4.0),
            constraints: Some(crate::raw::RawConstraints {
                horizontal: Some("MAX".into()),
                vertical: Some("MIN".into()),
            }),
            visible: true,
            ..Default::default()
        };
        let parent = RawNode {
            id: "1:2".into(),
            kind: "FRAME".into(),
            layout_mode: Some("HORIZONTAL".into()),
            visible: true,
            children: vec![child],
            ..Default::default()
        };
        let node = build(&parent);
        match &node.children[0].placement {
            Placement::Absolute { x, y, anchor_h, .. } => {
                assert_eq!(*x, 8.0);
                assert_eq!(*y, 4.0);
                assert_eq!(*anchor_h, Anchor::Max);
            }
            other => panic!("expected absolute, got {other:?}"),
        }
    }

    #[test]
    fn bound_variable_wins_over_literal_hex() {
        let fill = Fill {
            value: "#0d99ff".into(),
            token: Some("color/brand/primary".into()),
            css_var: Some("--color-brand-primary".into()),
            kind: "SOLID".into(),
            image_hash: None,
        };
        assert_eq!(fill.css_value(), "var(--color-brand-primary, #0d99ff)");
    }

    #[test]
    fn invisible_children_are_dropped() {
        let parent = RawNode {
            id: "1:2".into(),
            kind: "FRAME".into(),
            visible: true,
            children: vec![
                RawNode { id: "a".into(), visible: false, ..Default::default() },
                RawNode { id: "b".into(), visible: true, ..Default::default() },
            ],
            ..Default::default()
        };
        let node = build(&parent);
        assert_eq!(node.children.len(), 1);
        assert_eq!(node.children[0].id, "b");
    }
}
