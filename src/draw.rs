//! From a grid or a string to PNG bands sized in whole terminal cells.

use crate::fonts::{self, FacePair, Fonts, Rasterizer, Shaper};
use crate::layout::{Block, Geometry, Layout, Settings};
use crate::model::Grid;
use crate::paint::{self, Canvas, Painter};
use crate::style::Theme;
use crate::text::{self, TextBlock};

/// The terminal a drawing is for.
#[derive(Clone, Copy, Debug)]
pub struct Screen {
    /// One cell, width and height in the pixels the terminal reports.
    pub cell: (u32, u32),
    pub columns: u32,
    pub rows: u32,
}

impl Screen {
    /// For `--png`, with no terminal to ask.
    pub const OFFLINE: Screen = Screen {
        cell: (8, 16),
        columns: 160,
        rows: 50,
    };

    /// Image pixels per cell. Terminals that report small cells are
    /// counting points on a HiDPI screen, so the image gets twice the pixels
    /// and the terminal fits them into the same cells.
    pub fn cell_pixels(&self) -> (u32, u32) {
        let scale = if self.cell.1 < 24 { 2 } else { 1 };
        (self.cell.0.max(1) * scale, self.cell.1.max(1) * scale)
    }
}

/// What a drawing shows.
pub enum Body {
    Table {
        block: Block,
        geometry: Geometry,
    },
    /// A string, as terminal text.
    Text(TextBlock),
}

/// A laid-out value and the cells it covers.
pub struct Drawing {
    pub body: Body,
    /// Image pixels per cell.
    pub cell: (u32, u32),
    /// Size in cells.
    pub columns: u32,
    pub rows: u32,
}

/// Lay `grid` out for `screen`, with text about the size of the terminal's.
///
/// # Errors
///
/// Fails when the font has no metrics.
pub fn lay_out(
    grid: &Grid,
    fonts: &Fonts,
    faces: FacePair,
    theme: &Theme,
    screen: &Screen,
    settings: Settings,
) -> Result<Drawing, String> {
    let cell = screen.cell_pixels();
    // A terminal's cell is about 1.2 times its font size tall.
    let px = (cell.1 as f32 * 0.8).round();
    let metrics = fonts::metrics(fonts, faces.regular, px).ok_or("the font has no metrics")?;
    let mut geometry = Geometry::new(px, metrics);
    // A padding character is the card look's spacing, about 0.6 em.
    geometry.pad_left = (settings.padding.0 as f32 * px * 0.6).round();
    geometry.pad_right = (settings.padding.1 as f32 * px * 0.6).round();
    if settings.border.is_some() {
        // A nested table's frame needs room from the lines around it.
        geometry.inset = geometry.pad_y;
    }
    let mut shaper = Shaper::new(fonts, px);
    let block = Layout {
        shaper: &mut shaper,
        faces,
        geometry,
        theme,
        settings,
    }
    .block(grid, max_width(screen));
    Ok(Drawing {
        columns: (block.width / cell.0 as f32).ceil() as u32,
        rows: (block.height / cell.1 as f32).ceil() as u32,
        body: Body::Table { block, geometry },
        cell,
    })
}

/// Lay a string out as terminal text in `faces`, which should be monospace.
///
/// # Errors
///
/// Fails when the font has no metrics.
pub fn lay_out_text(
    text: &str,
    fonts: &Fonts,
    faces: FacePair,
    theme: &Theme,
    screen: &Screen,
) -> Result<Drawing, String> {
    let cell = screen.cell_pixels();
    let block = text::lay_out(
        text,
        fonts,
        faces,
        theme,
        (cell.0 as f32, cell.1 as f32),
        max_width(screen),
    )?;
    Ok(Drawing {
        columns: (block.width / cell.0 as f32).ceil() as u32,
        rows: (block.height / cell.1 as f32).ceil() as u32,
        body: Body::Text(block),
        cell,
    })
}

/// The widest a drawing may be, in image pixels: the screen less a column,
/// so the image never touches the last one.
fn max_width(screen: &Screen) -> f32 {
    (screen.columns.saturating_sub(1).max(20) * screen.cell_pixels().0) as f32
}

/// Paint rows `first .. first + count` of the drawing's cells.
pub fn band(
    drawing: &Drawing,
    theme: &Theme,
    rasterizer: &mut Rasterizer<'_>,
    first: u32,
    count: u32,
) -> Canvas {
    let mut canvas = Canvas::new(
        drawing.columns * drawing.cell.0,
        count * drawing.cell.1,
        (first * drawing.cell.1) as i32,
    );
    match &drawing.body {
        Body::Table { block, geometry } => Painter {
            theme,
            geometry: *geometry,
            rasterizer,
        }
        .block(&mut canvas, block, 0.0, 0.0, false),
        Body::Text(block) => paint::text(&mut canvas, block, theme, rasterizer),
    }
    canvas
}

/// A finished band: its index, its height in cells, and what `finish` made.
type Painted<T> = (usize, u32, T);

/// Paint the drawing in bands of `rows` cell rows on every core, and turn
/// each band into a `T` with `finish`. The results come back in order, each
/// with its band's height in cells.
///
/// # Errors
///
/// Fails when `finish` fails for any band, or a painting thread panics.
pub fn paint_bands<T: Send>(
    drawing: &Drawing,
    theme: &Theme,
    fonts: &Fonts,
    rows: u32,
    finish: impl Fn(Canvas) -> Result<T, String> + Sync,
) -> Result<Vec<(u32, T)>, String> {
    let rows = rows.max(1);
    let count = drawing.rows.div_ceil(rows) as usize;
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .clamp(1, count.max(1));
    let finish = &finish;
    let painted: Vec<Result<Vec<Painted<T>>, String>> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|worker| {
                scope.spawn(move || {
                    // Each thread keeps its own glyph cache. Bands are dealt
                    // out in turn, so busy and quiet parts of the table
                    // spread over all threads.
                    let mut rasterizer = Rasterizer::new(fonts);
                    (worker..count)
                        .step_by(threads)
                        .map(|index| {
                            let first = index as u32 * rows;
                            let height = rows.min(drawing.rows - first);
                            let canvas = band(drawing, theme, &mut rasterizer, first, height);
                            finish(canvas).map(|done| (index, height, done))
                        })
                        .collect()
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|_| Err("a painting thread panicked".into()))
            })
            .collect()
    });
    let mut bands = Vec::with_capacity(count);
    for result in painted {
        bands.extend(result?);
    }
    bands.sort_by_key(|(index, ..)| *index);
    Ok(bands
        .into_iter()
        .map(|(_, height, done)| (height, done))
        .collect())
}

/// Cell rows per band when `--png` paints a whole drawing.
const PNG_BAND_ROWS: u32 = 64;

/// The band as a PNG.
///
/// # Errors
///
/// Fails when the encoder does.
pub fn png(canvas: &Canvas) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut writer = encoder(&mut out, canvas.width, canvas.height)
        .write_header()
        .map_err(png_error)?;
    writer.write_image_data(&canvas.rgba).map_err(png_error)?;
    writer.finish().map_err(png_error)?;
    Ok(out)
}

/// The size of the drawing itself, in pixels, without the transparent
/// margin that rounds it up to whole cells for the terminal.
pub fn image_size(drawing: &Drawing) -> (u32, u32) {
    let (width, height) = match &drawing.body {
        Body::Table { block, .. } => (block.width, block.height),
        Body::Text(block) => (block.width, block.height),
    };
    (
        (width.ceil() as u32).clamp(1, drawing.columns * drawing.cell.0),
        (height.ceil() as u32).clamp(1, drawing.rows * drawing.cell.1),
    )
}

/// Copy the rows of `canvas` that fall inside an image `width` × `height`
/// pixels, each cut to `width`, onto `out`.
fn crop_into(
    canvas: &Canvas,
    width: u32,
    height: u32,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    let stride = canvas.width as usize * 4;
    let keep = width as usize * 4;
    let rows = (height as i64 - i64::from(canvas.top)).clamp(0, i64::from(canvas.height)) as usize;
    for row in canvas.rgba.chunks_exact(stride).take(rows) {
        out.write_all(&row[..keep])?;
    }
    Ok(())
}

/// The whole drawing as one PNG, cut to [`image_size`]. It is painted and
/// encoded a band at a time, so memory stays at one band however tall the
/// drawing is.
///
/// # Errors
///
/// Fails when the encoder does.
pub fn whole_png(
    drawing: &Drawing,
    theme: &Theme,
    rasterizer: &mut Rasterizer<'_>,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let (width, height) = image_size(drawing);
    let mut writer = encoder(&mut out, width, height)
        .write_header()
        .map_err(png_error)?;
    let mut stream = writer.stream_writer().map_err(png_error)?;
    let mut first = 0;
    while first < drawing.rows {
        let count = PNG_BAND_ROWS.min(drawing.rows - first);
        let canvas = band(drawing, theme, rasterizer, first, count);
        crop_into(&canvas, width, height, &mut stream)
            .map_err(|err| format!("encoding a PNG: {err}"))?;
        first += count;
    }
    stream.finish().map_err(png_error)?;
    writer.finish().map_err(png_error)?;
    Ok(out)
}

/// The whole drawing as straight RGBA, cut to [`image_size`], painted on
/// every core. Returns the width, the height, and the pixels.
///
/// # Errors
///
/// Fails when a painting thread does.
pub fn whole_rgba(
    drawing: &Drawing,
    theme: &Theme,
    fonts: &Fonts,
) -> Result<(u32, u32, Vec<u8>), String> {
    let (width, height) = image_size(drawing);
    let bands = paint_bands(drawing, theme, fonts, PNG_BAND_ROWS, |canvas| {
        let mut rows = Vec::new();
        crop_into(&canvas, width, height, &mut rows)
            .map_err(|err| format!("copying pixels: {err}"))?;
        Ok(rows)
    })?;
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for (_, rows) in bands {
        rgba.extend_from_slice(&rows);
    }
    Ok((width, height, rgba))
}

fn encoder<'a, W: std::io::Write + 'a>(out: W, width: u32, height: u32) -> png::Encoder<'a, W> {
    let mut encoder = png::Encoder::new(out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder
}

fn png_error(err: png::EncodingError) -> String {
    format!("encoding a PNG: {err}")
}
