//! A table painted in bands must match the same table painted whole, pixel
//! for pixel, or the bands show seams in the terminal.

use nu_plugin_render::draw::{self, Body, Drawing, Screen};
use nu_plugin_render::fonts::{Fonts, Rasterizer};
use nu_plugin_render::model::{Align, Cell, Content, Grid};
use nu_plugin_render::style::Theme;

fn text(theme: &Theme, text: &str) -> Cell {
    Cell {
        content: Content::Text(text.to_string()),
        color: theme.text,
        bold: false,
        background: None,
        center: false,
    }
}

fn nested(theme: &Theme) -> Cell {
    let grid = Grid {
        header: Some(vec![text(theme, "k"), text(theme, "v")]),
        index: None,
        rows: (0..3)
            .map(|i| {
                vec![
                    text(theme, &format!("key {i}")),
                    text(theme, &format!("{i}.5")),
                ]
            })
            .collect(),
        align: vec![Align::Left, Align::Decimal],
    };
    Cell {
        content: Content::Grid {
            grid: Box::new(grid),
            summary: "[table 3 rows]".to_string(),
        },
        color: theme.text,
        bold: false,
        background: None,
        center: false,
    }
}

/// Rows of plain text, two-line cells, nested tables, emoji, and numbers.
fn table(theme: &Theme) -> Grid {
    let rows = (0..40)
        .map(|i| {
            let middle = match i % 7 {
                3 => nested(theme),
                1 | 5 => text(theme, "two\nlines"),
                2 => text(theme, "emoji 🚀 and 日本語"),
                _ => text(theme, "plain"),
            };
            vec![
                text(theme, &format!("row {i}")),
                middle,
                text(theme, &(i * 37).to_string()),
            ]
        })
        .collect();
    Grid {
        header: Some(vec![
            text(theme, "name"),
            text(theme, "value"),
            text(theme, "n"),
        ]),
        index: Some((0..40).map(|i| text(theme, &i.to_string())).collect()),
        rows,
        align: vec![Align::Left, Align::Left, Align::Right],
    }
}

fn paint_in_bands(
    drawing: &Drawing,
    theme: &Theme,
    rasterizer: &mut Rasterizer<'_>,
    rows: u32,
) -> Vec<u8> {
    let mut joined = Vec::new();
    let mut first = 0;
    while first < drawing.rows {
        let count = rows.min(drawing.rows - first);
        joined.extend(draw::band(drawing, theme, rasterizer, first, count).rgba);
        first += count;
    }
    joined
}

/// Paint the drawing whole, then in bands of several sizes, and check
/// that the joined bands are the same pixels.
fn assert_bands_match(drawing: &Drawing, theme: &Theme, fonts: &Fonts) {
    let mut rasterizer = Rasterizer::new(fonts);
    let whole = draw::band(drawing, theme, &mut rasterizer, 0, drawing.rows).rgba;
    assert!(
        whole.iter().any(|&byte| byte != 0),
        "the drawing drew nothing"
    );
    for rows in [1, 2, 7, 28] {
        let joined = paint_in_bands(drawing, theme, &mut rasterizer, rows);
        assert_eq!(joined.len(), whole.len());
        let first_difference = joined.iter().zip(&whole).position(|(a, b)| a != b);
        assert_eq!(
            first_difference, None,
            "bands of {rows} rows differ from the whole drawing"
        );
    }
}

#[test]
fn bands_join_into_the_whole_table() {
    let fonts = Fonts::load().expect("system fonts");
    let theme = Theme::dark();
    let screen = Screen {
        cell: (8, 16),
        columns: 100,
        rows: 30,
    };
    let drawing =
        draw::lay_out(&table(&theme), &fonts, fonts.sans, &theme, &screen).expect("layout");
    assert_bands_match(&drawing, &theme, &fonts);
}

/// Lines like `grid -c -i`'s: an icon, colors, a highlighted name, rules,
/// and wide characters. Some are wider than a 40-column screen, so the text
/// shrinks and the longest lines wrap.
fn terminal_text() -> String {
    (0..50)
        .map(|i| {
            format!(
                "\x1b[38;2;87;199;255m\u{f07b} dir{i:02}\x1b[0m  │ \x1b[48;2;243;249;157;38;2;40;42;54mREADME.md\x1b[0m │ 日本語 {}",
                "x".repeat(i % 30)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn text_bands_join_into_the_whole_text() {
    let fonts = Fonts::load().expect("system fonts");
    let theme = Theme::dark();
    let screen = Screen {
        cell: (8, 16),
        columns: 40,
        rows: 20,
    };
    let drawing =
        draw::lay_out_text(&terminal_text(), &fonts, fonts.mono, &theme, &screen).expect("layout");
    let Body::Text(block) = &drawing.body else {
        panic!("a string should lay out as text");
    };
    assert!(block.lines.len() > 50, "the longest lines should wrap");
    assert!(
        drawing.columns < screen.columns,
        "the text should fit the screen"
    );
    assert_bands_match(&drawing, &theme, &fonts);
}

#[test]
fn streamed_png_matches_the_whole_drawing() {
    let fonts = Fonts::load().expect("system fonts");
    let theme = Theme::dark();
    let screen = Screen {
        cell: (8, 16),
        columns: 100,
        rows: 30,
    };
    // Tall enough for several of the PNG writer's bands.
    let drawing = draw::lay_out_text(
        &terminal_text().repeat(3),
        &fonts,
        fonts.mono,
        &theme,
        &screen,
    )
    .expect("layout");
    let mut rasterizer = Rasterizer::new(&fonts);
    let whole = draw::band(&drawing, &theme, &mut rasterizer, 0, drawing.rows).rgba;
    let png = draw::whole_png(&drawing, &theme, &mut rasterizer).expect("png");

    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .expect("a readable PNG");
    let mut pixels = vec![0; reader.output_buffer_size().expect("a sized PNG")];
    let frame = reader.next_frame(&mut pixels).expect("a frame");
    let (width, height) = draw::image_size(&drawing);
    assert_eq!((frame.width, frame.height), (width, height));
    assert!(
        frame.height > 64 * drawing.cell.1,
        "should span several bands"
    );

    // The whole drawing, cut to the image's size the same way.
    let stride = (drawing.columns * drawing.cell.0) as usize * 4;
    let cropped: Vec<u8> = whole
        .chunks_exact(stride)
        .take(height as usize)
        .flat_map(|row| &row[..width as usize * 4])
        .copied()
        .collect();
    assert!(
        pixels[..frame.buffer_size()] == cropped[..],
        "the PNG's pixels differ"
    );

    let (rgba_width, rgba_height, rgba) = draw::whole_rgba(&drawing, &theme, &fonts).expect("rgba");
    assert_eq!((rgba_width, rgba_height), (width, height));
    assert!(rgba == cropped, "the parallel painting differs");
}
