//! IR -> React + CSS.
//!
//! The rule that matters most: when a node is an instance of a known component, we
//! emit the component with its variant props and do **not** recurse into it. That
//! single decision is the difference between `<Button variant="primary">Save</Button>`
//! and forty lines of nested divs that happen to look like a button.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::css::{self, Decls};
use crate::ir::{Layout, Node, Token, pascal_case};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum StyleMode {
    /// A `.module.css` file with semantic class names.
    #[default]
    CssModules,
    /// Tailwind-ish utility classes inline. Falls back to arbitrary values.
    Tailwind,
    /// React `style={{}}` objects — no separate file.
    Inline,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedFile {
    pub path: String,
    pub contents: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Generated {
    pub component_name: String,
    pub files: Vec<GeneratedFile>,
    /// Components referenced but not defined here — the caller already has these.
    pub imported_components: Vec<String>,
    /// Vector/image nodes that need `export_assets` to be runnable.
    pub pending_assets: Vec<String>,
    pub node_count: usize,
    pub warnings: Vec<String>,
}

struct Ctx {
    mode: StyleMode,
    /// class name -> declarations, in emission order.
    rules: Vec<(String, Decls)>,
    used: HashMap<String, usize>,
    components: BTreeSet<String>,
    assets: Vec<String>,
    warnings: Vec<String>,
}

impl Ctx {
    /// Stable, readable, collision-free class name from a Figma layer name.
    fn class_for(&mut self, node: &Node) -> String {
        let base = kebab_case(&node.name);
        let base = if base.is_empty() {
            kebab_case(&node.kind)
        } else {
            base
        };
        let n = self.used.entry(base.clone()).or_insert(0);
        *n += 1;
        if *n == 1 { base } else { format!("{base}-{n}") }
    }
}

pub fn generate(
    root: &Node,
    tokens: &[Token],
    mode: StyleMode,
    component_name: Option<&str>,
) -> Generated {
    let name = component_name
        .map(pascal_case)
        .unwrap_or_else(|| pascal_case(&root.name));

    let mut ctx = Ctx {
        mode,
        rules: Vec::new(),
        used: HashMap::new(),
        components: BTreeSet::new(),
        assets: Vec::new(),
        warnings: Vec::new(),
    };

    let body = emit(root, None, &mut ctx, 3, true);
    let node_count = root.count();

    let mut files = Vec::new();
    let sheet_name = match mode {
        StyleMode::CssModules => Some(format!("{name}.module.css")),
        StyleMode::Tailwind if !ctx.rules.is_empty() => Some(format!("{name}.css")),
        _ => None,
    };
    let style_import = match (&sheet_name, mode) {
        (Some(f), StyleMode::CssModules) => format!("import styles from \"./{f}\";\n"),
        (Some(f), _) => format!("import \"./{f}\";\n"),
        (None, _) => String::new(),
    };

    let imports: Vec<String> = ctx.components.iter().cloned().collect();
    let component_imports = if imports.is_empty() {
        String::new()
    } else {
        imports
            .iter()
            .map(|c| format!("import {{ {c} }} from \"./components/{c}\";\n"))
            .collect::<Vec<_>>()
            .join("")
    };

    let tsx = format!(
        "// Generated from Figma by figma-canvas-mcp. Re-generating overwrites this file.\n\
         {component_imports}{style_import}\n\
         export interface {name}Props {{\n  className?: string;\n}}\n\n\
         export function {name}({{ className }}: {name}Props) {{\n  return (\n{body}\n  );\n}}\n\n\
         export default {name};\n"
    );
    files.push(GeneratedFile {
        path: format!("{name}.tsx"),
        contents: tsx,
    });

    if let Some(sheet_file) = &sheet_name {
        let mut sheet = String::from("/* Generated from Figma by figma-canvas-mcp. */\n");
        for (class, decls) in &ctx.rules {
            if decls.is_empty() {
                continue;
            }
            sheet.push_str(&format!(
                ".{class} {{\n{}\n}}\n\n",
                css::to_block(decls, "  ")
            ));
        }
        files.push(GeneratedFile {
            path: sheet_file.clone(),
            contents: sheet,
        });
    }

    if !tokens.is_empty() {
        files.push(GeneratedFile {
            path: "tokens.css".into(),
            contents: tokens_css(tokens),
        });
    }

    Generated {
        component_name: name,
        files,
        imported_components: imports,
        pending_assets: ctx.assets.clone(),
        node_count,
        warnings: ctx.warnings.clone(),
    }
}

fn emit(
    node: &Node,
    parent: Option<&Layout>,
    ctx: &mut Ctx,
    indent: usize,
    is_root: bool,
) -> String {
    let pad = "  ".repeat(indent);

    // 1. A component instance keeps its identity. Do not walk inside it.
    if let Some(inst) = &node.instance {
        let tag = inst.tag();
        ctx.components.insert(tag.clone());

        // The component styles itself, but how it sits in this layout is ours to
        // say. Pass only placement through, on the className nearly every design
        // system component already accepts.
        let placement = css::placement_only(css::declarations(node, parent));
        let class_attr = if placement.is_empty() {
            String::new()
        } else {
            let class = ctx.class_for(node);
            match ctx.mode {
                StyleMode::Inline => format!(" style={{{{{}}}}}", inline_style(&placement)),
                StyleMode::CssModules => {
                    ctx.rules.push((class.clone(), placement));
                    format!(" className={{styles.{}}}", js_ident(&class))
                }
                StyleMode::Tailwind => {
                    let (mut utils, leftover) = tailwind(&placement);
                    if !leftover.is_empty() {
                        ctx.rules.push((class.clone(), leftover));
                        utils.push(class.clone());
                    }
                    format!(" className=\"{}\"", utils.join(" "))
                }
            }
        };

        let props = inst
            .props
            .iter()
            .map(|(k, v)| {
                let key = prop_key(k);
                if v == "true" || v == "false" {
                    format!(" {key}={{{v}}}")
                } else {
                    format!(" {key}=\"{}\"", escape_attr(v))
                }
            })
            .collect::<Vec<_>>()
            .join("");
        return format!("{pad}<{tag}{class_attr}{props} />");
    }

    // 2. Vectors become assets — CSS cannot honestly reproduce a bezier path.
    if node.is_vector() {
        let file = format!("{}.svg", kebab_case(&node.name));
        ctx.assets.push(node.id.clone());
        return format!(
            "{pad}<img src=\"./assets/{file}\" alt=\"{}\" width={{{}}} height={{{}}} />",
            escape_attr(&node.name),
            node.width.round(),
            node.height.round()
        );
    }

    let mut decls = css::declarations(node, parent);
    if is_root {
        decls = css::strip_root_position(decls);
    }
    let class = ctx.class_for(node);
    // The root element merges the caller's `className`, so the prop the generated
    // interface advertises is actually honoured.
    let merge = |own: String| -> String {
        if is_root {
            format!(" className={{[{own}, className].filter(Boolean).join(\" \")}}")
        } else {
            String::new()
        }
    };

    let attr = match ctx.mode {
        StyleMode::CssModules => {
            ctx.rules.push((class.clone(), decls));
            let own = format!("styles.{}", js_ident(&class));
            if is_root {
                merge(own)
            } else {
                format!(" className={{{own}}}")
            }
        }
        StyleMode::Inline => {
            let style = format!(" style={{{{{}}}}}", inline_style(&decls));
            if is_root {
                format!(" className={{className}}{style}")
            } else {
                style
            }
        }
        StyleMode::Tailwind => {
            let (mut utils, leftover) = tailwind(&decls);
            if !leftover.is_empty() {
                // No utility exists for these, so keep them in a stylesheet and
                // put the class on the element. Dropping them would silently
                // lose shadows, gradients and transforms.
                ctx.rules.push((class.clone(), leftover));
                utils.push(class.clone());
            }
            let joined = utils.join(" ");
            if is_root {
                merge(format!("\"{joined}\""))
            } else {
                format!(" className=\"{joined}\"",)
            }
        }
    };

    // 3. Pure text.
    if node.is_text_only() {
        let text = node.text.as_ref().map(|t| t.content.as_str()).unwrap_or("");
        let tag = text_tag(node);
        return format!("{pad}<{tag}{attr}>{}</{tag}>", escape_text(text));
    }

    // 4. Container.
    if node.children.is_empty() {
        return format!("{pad}<div{attr} />");
    }

    let inner: Vec<String> = node
        .children
        .iter()
        .map(|c| emit(c, Some(&node.layout), ctx, indent + 1, false))
        .collect();

    format!("{pad}<div{attr}>\n{}\n{pad}</div>", inner.join("\n"))
}

/// Guess a semantic tag from the layer name and type size.
fn text_tag(node: &Node) -> &'static str {
    let lower = node.name.to_lowercase();
    let size = node.text.as_ref().and_then(|t| t.font_size).unwrap_or(16.0);
    if lower.starts_with("h1") || size >= 36.0 {
        "h1"
    } else if lower.starts_with("h2") || size >= 28.0 {
        "h2"
    } else if lower.starts_with("h3") || size >= 22.0 {
        "h3"
    } else if lower.contains("label") {
        "span"
    } else {
        "p"
    }
}

fn tokens_css(tokens: &[Token]) -> String {
    let mut by_mode: BTreeMap<String, Vec<&Token>> = BTreeMap::new();
    for t in tokens {
        for mode in t.values_by_mode.keys() {
            by_mode.entry(mode.clone()).or_default().push(t);
        }
    }

    let mut out = String::from(
        "/* Design tokens lifted from Figma variables by figma-canvas-mcp.\n   \
         These come from the Plugin API, which is why they carry real names\n   \
         instead of the hex values a REST export would have flattened them to. */\n\n",
    );

    let modes: Vec<&String> = by_mode.keys().collect();
    for (i, mode) in modes.iter().enumerate() {
        // First mode is the default; later modes become data-theme blocks.
        let selector = if i == 0 {
            ":root".to_string()
        } else {
            format!(":root[data-theme=\"{}\"]", kebab_case(mode))
        };
        out.push_str(&format!("{selector} {{\n"));
        for t in by_mode.get(*mode).into_iter().flatten() {
            if let Some(v) = t.values_by_mode.get(*mode) {
                if let Some(desc) = &t.description {
                    if !desc.is_empty() {
                        out.push_str(&format!("  /* {} */\n", desc.replace("*/", "*\\/")));
                    }
                }
                out.push_str(&format!("  {}: {};\n", t.css_var, v));
            }
        }
        out.push_str("}\n\n");
    }
    out
}

fn inline_style(decls: &Decls) -> String {
    decls
        .iter()
        .map(|(k, v)| format!("{}: \"{}\"", camel_case(k), escape_attr(v)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Map the handful of declarations with clean Tailwind equivalents; return the
/// rest so they still land in a stylesheet rather than being silently dropped.
fn tailwind(decls: &Decls) -> (Vec<String>, Decls) {
    let mut utils = Vec::new();
    let mut leftover = Decls::new();
    for (k, v) in decls {
        let u = match (k.as_str(), v.as_str()) {
            ("display", "flex") => Some("flex".to_string()),
            ("display", "grid") => Some("grid".to_string()),
            ("flex-direction", "column") => Some("flex-col".to_string()),
            ("flex-wrap", "wrap") => Some("flex-wrap".to_string()),
            ("justify-content", "center") => Some("justify-center".to_string()),
            ("justify-content", "space-between") => Some("justify-between".to_string()),
            ("justify-content", "flex-end") => Some("justify-end".to_string()),
            ("align-items", "center") => Some("items-center".to_string()),
            ("align-items", "flex-end") => Some("items-end".to_string()),
            ("align-items", "baseline") => Some("items-baseline".to_string()),
            ("align-self", "stretch") => Some("self-stretch".to_string()),
            ("flex", "1 1 0%") => Some("flex-1".to_string()),
            ("flex-shrink", "0") => Some("shrink-0".to_string()),
            ("position", "absolute") => Some("absolute".to_string()),
            ("position", "relative") => Some("relative".to_string()),
            ("overflow", "hidden") => Some("overflow-hidden".to_string()),
            ("gap", val) => px_scale(val).map(|n| format!("gap-{n}")),
            ("padding", val) if !val.contains(' ') => px_scale(val).map(|n| format!("p-{n}")),
            _ => None,
        };
        match u {
            Some(u) => utils.push(u),
            None => leftover.push((k.clone(), v.clone())),
        }
    }
    (utils, leftover)
}

/// Tailwind's spacing scale is 0.25rem steps; 4px -> 1.
fn px_scale(v: &str) -> Option<String> {
    let n: f64 = v.strip_suffix("px")?.parse().ok()?;
    if n == 0.0 {
        return Some("0".into());
    }
    let steps = n / 4.0;
    if (steps - steps.round()).abs() < 0.01 {
        Some(format!("{}", steps.round() as i64))
    } else {
        Some(format!("[{v}]"))
    }
}

fn prop_key(figma_prop: &str) -> String {
    // Figma exposes variant keys like "Size" or "Has Icon#12:3".
    let base = figma_prop.split('#').next().unwrap_or(figma_prop);
    camel_case(&kebab_case(base))
}

pub fn kebab_case(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = true;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn camel_case(s: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for ch in s.chars() {
        if ch == '-' || ch == '_' || ch == ' ' {
            upper = true;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// CSS module classes are reached as `styles.x`, so the key must be a JS ident.
fn js_ident(class: &str) -> String {
    let c = camel_case(class);
    if c.is_empty() || c.starts_with(|ch: char| ch.is_ascii_digit()) {
        format!("c{c}")
    } else {
        c
    }
}

fn escape_attr(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "&quot;")
}

fn escape_text(s: &str) -> String {
    s.replace('{', "&#123;")
        .replace('}', "&#125;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Align, Direction, Edges, Instance, Justify, Placement, Sizing, Style, Text};

    fn frame(name: &str, children: Vec<Node>) -> Node {
        Node {
            id: format!("id-{name}"),
            name: name.into(),
            kind: "FRAME".into(),
            visible: true,
            width: 200.0,
            height: 80.0,
            layout: Layout::Flex {
                direction: Direction::Row,
                gap: 8.0,
                cross_gap: None,
                wrap: false,
                justify: Justify::Start,
                align: Align::Center,
            },
            placement: Placement::InFlow {
                width: Sizing::Hug,
                height: Sizing::Hug,
                align_self: None,
            },
            padding: Edges::default(),
            style: Style::default(),
            text: None,
            instance: None,
            figma_css: None,
            tokens: Default::default(),
            exportable: false,
            children,
        }
    }

    fn text_node(name: &str, content: &str, size: f64) -> Node {
        let mut n = frame(name, vec![]);
        n.kind = "TEXT".into();
        n.layout = Layout::Absolute;
        n.text = Some(Text {
            content: content.into(),
            font_family: Some("Inter".into()),
            font_size: Some(size),
            font_weight: Some(600.0),
            line_height: None,
            letter_spacing: None,
            align: None,
            transform: None,
            decoration: None,
            style_name: None,
        });
        n
    }

    #[test]
    fn instance_emits_component_with_variant_props_and_does_not_recurse() {
        let mut inner = text_node("label", "should not appear", 16.0);
        inner.name = "buried".into();
        let mut btn = frame("Save button", vec![inner]);
        btn.kind = "INSTANCE".into();
        btn.instance = Some(Instance {
            component: "Button".into(),
            component_id: Some("10:1".into()),
            props: BTreeMap::from([
                ("Variant".to_string(), "primary".to_string()),
                ("Size#9:0".to_string(), "md".to_string()),
                ("Has Icon".to_string(), "true".to_string()),
            ]),
            from_library: true,
        });

        let g = generate(&frame("Root", vec![btn]), &[], StyleMode::CssModules, None);
        let tsx = &g.files[0].contents;
        assert!(tsx.contains("<Button"), "expected component tag: {tsx}");
        assert!(tsx.contains("variant=\"primary\""));
        assert!(tsx.contains("size=\"md\""));
        assert!(tsx.contains("hasIcon={true}"), "booleans become JSX exprs");
        assert!(
            !tsx.contains("should not appear"),
            "must not walk inside an instance"
        );
        assert_eq!(g.imported_components, vec!["Button".to_string()]);
    }

    #[test]
    fn headings_get_semantic_tags_from_size() {
        let g = generate(
            &frame("Root", vec![text_node("Title", "Hello", 40.0)]),
            &[],
            StyleMode::CssModules,
            None,
        );
        assert!(g.files[0].contents.contains("<h1"));
    }

    #[test]
    fn duplicate_layer_names_get_unique_classes() {
        let root = frame(
            "Root",
            vec![
                frame("row", vec![]),
                frame("row", vec![]),
                frame("row", vec![]),
            ],
        );
        let g = generate(&root, &[], StyleMode::CssModules, None);
        let sheet = &g.files[1].contents;
        assert!(sheet.contains(".row {"));
        assert!(sheet.contains(".row-2 {"));
        assert!(sheet.contains(".row-3 {"));
    }

    #[test]
    fn jsx_braces_in_text_are_escaped() {
        let g = generate(
            &frame("Root", vec![text_node("t", "price {total} <b>", 16.0)]),
            &[],
            StyleMode::CssModules,
            None,
        );
        let tsx = &g.files[0].contents;
        assert!(tsx.contains("&#123;total&#125;"));
        assert!(!tsx.contains("<b>"));
    }

    #[test]
    fn tokens_become_a_themed_stylesheet() {
        let tokens = vec![Token {
            id: "V:1".into(),
            name: "color/brand/primary".into(),
            css_var: "--color-brand-primary".into(),
            kind: "COLOR".into(),
            collection: Some("Palette".into()),
            values_by_mode: BTreeMap::from([
                ("Light".to_string(), "#0d99ff".to_string()),
                ("Dark".to_string(), "#7cc4ff".to_string()),
            ]),
            description: None,
        }];
        let g = generate(&frame("Root", vec![]), &tokens, StyleMode::CssModules, None);
        let css = g
            .files
            .iter()
            .find(|f| f.path == "tokens.css")
            .expect("tokens.css emitted");
        assert!(css.contents.contains("--color-brand-primary: #0d99ff;"));
        assert!(
            css.contents.contains("[data-theme=\"light\"]") || css.contents.contains(":root {")
        );
        assert!(css.contents.contains("#7cc4ff"));
    }

    #[test]
    fn tailwind_mode_keeps_unmappable_declarations_in_a_sheet() {
        let mut n = frame("card", vec![]);
        n.style.shadows = vec![crate::ir::Shadow {
            inset: false,
            x: 0.0,
            y: 2.0,
            blur: 8.0,
            spread: 0.0,
            color: "rgb(0 0 0 / 0.150)".into(),
        }];
        let g = generate(&frame("Root", vec![n]), &[], StyleMode::Tailwind, None);
        let tsx = &g.files[0].contents;
        assert!(tsx.contains("flex"));
        assert!(tsx.contains("items-center"));
        assert!(tsx.contains("gap-2"), "8px maps to Tailwind's 2 step");
        // box-shadow has no clean utility, so it must survive in CSS.
        assert!(g.files.iter().any(|f| f.contents.contains("box-shadow")));
    }

    #[test]
    fn vectors_are_exported_not_faked() {
        let mut v = frame("Logo mark", vec![]);
        v.kind = "VECTOR".into();
        let g = generate(&frame("Root", vec![v]), &[], StyleMode::CssModules, None);
        assert!(g.files[0].contents.contains("./assets/logo-mark.svg"));
        assert_eq!(g.pending_assets.len(), 1);
    }
}
