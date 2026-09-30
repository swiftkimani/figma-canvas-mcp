//! Scratch measurement of tool payload sizes.
use figma_canvas_mcp::{ir, raw::*};

fn leaf(i: usize) -> RawNode {
    RawNode {
        id: format!("10:{i}"),
        name: format!("Row item {i}"),
        kind: "FRAME".into(),
        visible: true,
        absolute_bounding_box: Some(RawRect {
            x: 0.0,
            y: 0.0,
            width: 280.0,
            height: 48.0,
        }),
        layout_mode: Some("HORIZONTAL".into()),
        item_spacing: Some(8.0),
        primary_axis_align_items: Some("SPACE_BETWEEN".into()),
        counter_axis_align_items: Some("CENTER".into()),
        padding: Some(RawPadding {
            top: 8.0,
            right: 12.0,
            bottom: 8.0,
            left: 12.0,
        }),
        layout_sizing_horizontal: Some("FILL".into()),
        layout_sizing_vertical: Some("HUG".into()),
        corner_radius: Some(RawRadius {
            top_left: 8.0,
            top_right: 8.0,
            bottom_right: 8.0,
            bottom_left: 8.0,
        }),
        fills: vec![RawPaint {
            kind: "SOLID".into(),
            visible: Some(true),
            opacity: Some(1.0),
            color: Some(RawColor {
                r: 0.95,
                g: 0.96,
                b: 0.98,
                a: None,
            }),
            variable_name: Some("color/surface/subtle".into()),
            variable_id: Some("V:9".into()),
            ..Default::default()
        }],
        children: vec![
            RawNode {
                id: format!("10:{i}a"),
                name: "Label".into(),
                kind: "TEXT".into(),
                visible: true,
                absolute_bounding_box: Some(RawRect {
                    x: 0.0,
                    y: 0.0,
                    width: 120.0,
                    height: 20.0,
                }),
                text: Some(RawText {
                    characters: format!("Item number {i}"),
                    font_family: Some("Inter".into()),
                    font_size: Some(14.0),
                    font_weight: Some(500.0),
                    line_height: Some("20px".into()),
                    text_align_horizontal: Some("LEFT".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            RawNode {
                id: format!("10:{i}b"),
                name: "Chevron".into(),
                kind: "VECTOR".into(),
                visible: true,
                absolute_bounding_box: Some(RawRect {
                    x: 0.0,
                    y: 0.0,
                    width: 16.0,
                    height: 16.0,
                }),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

fn main() {
    let root = RawNode {
        id: "1:1".into(),
        name: "Settings list".into(),
        kind: "FRAME".into(),
        visible: true,
        absolute_bounding_box: Some(RawRect {
            x: 0.0,
            y: 0.0,
            width: 320.0,
            height: 800.0,
        }),
        layout_mode: Some("VERTICAL".into()),
        item_spacing: Some(12.0),
        padding: Some(RawPadding {
            top: 16.0,
            right: 16.0,
            bottom: 16.0,
            left: 16.0,
        }),
        children: (0..50).map(leaf).collect(),
        ..Default::default()
    };
    let scene = ir::build(&root);
    let pretty = serde_json::to_string_pretty(std::slice::from_ref(&scene)).unwrap();
    let compact = serde_json::to_string(std::slice::from_ref(&scene)).unwrap();
    let outline = figma_canvas_mcp::outline::render(
        std::slice::from_ref(&scene),
        &figma_canvas_mcp::outline::Budget::default(),
    );

    let tok = |n: usize| n / 4;
    println!("nodes:                  {}", scene.count());
    println!();
    println!(
        "full, pretty:      {:>8} B  ~{:>6} tokens",
        pretty.len(),
        tok(pretty.len())
    );
    println!(
        "full, compact:     {:>8} B  ~{:>6} tokens",
        compact.len(),
        tok(compact.len())
    );
    println!(
        "OUTLINE (default): {:>8} B  ~{:>6} tokens",
        outline.len(),
        tok(outline.len())
    );
    println!();
    println!(
        "outline vs full-pretty: {:.0}x cheaper",
        pretty.len() as f64 / outline.len() as f64
    );
    println!("\n--- the entire outline ---");
    println!("{outline}");
}
