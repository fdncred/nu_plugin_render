//! Drawing a measured table into RGBA pixels, one horizontal band at a time,
//! so a long table never needs one huge image.

use swash::scale::image::{Content, Image};

use crate::fonts::Rasterizer;
use crate::layout::{Block, Geometry, Placed};
use crate::style::{Rgba, Theme};
use crate::text::{Rule, TextBlock};

/// A band of pixels: the rows from `top` to `top + height` of the whole
/// table. Drawing calls take whole-table coordinates and clip to the band.
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub top: i32,
    /// Straight-alpha RGBA, row by row.
    pub rgba: Vec<u8>,
}

/// Which corners of a rectangle are rounded.
#[derive(Clone, Copy)]
pub struct Corners {
    pub top: bool,
    pub bottom: bool,
}

impl Corners {
    pub const ALL: Corners = Corners {
        top: true,
        bottom: true,
    };
    pub const NONE: Corners = Corners {
        top: false,
        bottom: false,
    };
}

/// A rectangle in whole-table pixels.
#[derive(Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Canvas {
    /// A transparent band.
    pub fn new(width: u32, height: u32, top: i32) -> Self {
        Canvas {
            width,
            height,
            top,
            rgba: vec![0; width as usize * height as usize * 4],
        }
    }

    fn bottom(&self) -> i32 {
        self.top + self.height as i32
    }

    /// Whether anything from `y` to `y + height` falls in the band.
    fn touches(&self, y: f32, height: f32) -> bool {
        y + height >= self.top as f32 && y < self.bottom() as f32
    }

    /// Draw `color` over pixel (x, y) at `coverage`, 0 to 1.
    fn blend(&mut self, x: i32, y: i32, color: Rgba, coverage: f32) {
        if x < 0 || x >= self.width as i32 || y < self.top || y >= self.bottom() {
            return;
        }
        let alpha = f32::from(color.0[3]) / 255.0 * coverage;
        if alpha <= 0.0 {
            return;
        }
        let at = ((y - self.top) as usize * self.width as usize + x as usize) * 4;
        let pixel = &mut self.rgba[at..at + 4];
        let below = f32::from(pixel[3]) / 255.0;
        let out = alpha + below * (1.0 - alpha);
        for (dst, &src) in pixel.iter_mut().zip(&color.0).take(3) {
            let (src, below_color) = (f32::from(src), f32::from(*dst));
            *dst = ((src * alpha + below_color * below * (1.0 - alpha)) / out).round() as u8;
        }
        pixel[3] = (out * 255.0).round() as u8;
    }

    /// Fill a rectangle with anti-aliased round corners of radius `r`.
    pub fn round_rect(&mut self, rect: Rect, r: f32, corners: Corners, color: Rgba) {
        let (x0, y0) = (rect.x.round() as i32, rect.y.round() as i32);
        let (x1, y1) = (
            (rect.x + rect.w).round() as i32,
            (rect.y + rect.h).round() as i32,
        );
        let (left, right) = (x0.max(0), x1.min(self.width as i32));
        let reach = r.ceil() as i32;
        for py in y0.max(self.top)..y1.min(self.bottom()) {
            let in_corner_rows =
                (corners.top && py < y0 + reach) || (corners.bottom && py >= y1 - reach);
            if !in_corner_rows && color.0[3] == 255 {
                // Opaque and square on this row: copy the color straight in.
                let start = ((py - self.top) as usize * self.width as usize + left as usize) * 4;
                let end = start + (right - left).max(0) as usize * 4;
                for pixel in self.rgba[start..end].as_chunks_mut::<4>().0 {
                    *pixel = color.0;
                }
                continue;
            }
            for px in left..right {
                let coverage = if in_corner_rows {
                    corner_coverage(px, py, (x0, y0, x1, y1), r, corners)
                } else {
                    1.0
                };
                if coverage > 0.0 {
                    self.blend(px, py, color, coverage);
                }
            }
        }
    }

    /// Draw a glyph with its origin at (x, baseline).
    pub fn glyph(&mut self, image: &Image, x: f32, baseline: f32, color: Rgba) {
        let left = x.round() as i32 + image.placement.left;
        let top = baseline.round() as i32 - image.placement.top;
        let (w, h) = (image.placement.width as i32, image.placement.height as i32);
        if top + h < self.top || top >= self.bottom() {
            return;
        }
        for row in 0..h {
            for col in 0..w {
                let at = (row * w + col) as usize;
                match image.content {
                    Content::Mask => {
                        let coverage = f32::from(image.data[at]) / 255.0;
                        self.blend(left + col, top + row, color, coverage);
                    }
                    Content::Color => {
                        let px = &image.data[at * 4..at * 4 + 4];
                        self.blend(
                            left + col,
                            top + row,
                            Rgba([px[0], px[1], px[2], px[3]]),
                            1.0,
                        );
                    }
                    Content::SubpixelMask => {
                        let px = &image.data[at * 4..at * 4 + 3];
                        let coverage = (f32::from(px[0]) + f32::from(px[1]) + f32::from(px[2]))
                            / (3.0 * 255.0);
                        self.blend(left + col, top + row, color, coverage);
                    }
                }
            }
        }
    }
}

/// How much of pixel (px, py) lies inside the rectangle once its corners
/// are rounded.
fn corner_coverage(px: i32, py: i32, rect: (i32, i32, i32, i32), r: f32, corners: Corners) -> f32 {
    let (x0, y0, x1, y1) = (rect.0 as f32, rect.1 as f32, rect.2 as f32, rect.3 as f32);
    let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
    let top = corners.top && cy < y0 + r;
    let bottom = corners.bottom && cy > y1 - r;
    let left = cx < x0 + r;
    let right = cx > x1 - r;
    let center = match (top, bottom, left, right) {
        (true, _, true, _) => (x0 + r, y0 + r),
        (true, _, _, true) => (x1 - r, y0 + r),
        (_, true, true, _) => (x0 + r, y1 - r),
        (_, true, _, true) => (x1 - r, y1 - r),
        _ => return 1.0,
    };
    let distance = ((cx - center.0).powi(2) + (cy - center.1).powi(2)).sqrt();
    (r - distance + 0.5).clamp(0.0, 1.0)
}

/// Paints blocks onto a canvas.
pub struct Painter<'a, 'f> {
    pub theme: &'a Theme,
    pub geometry: Geometry,
    pub rasterizer: &'a mut Rasterizer<'f>,
}

impl Painter<'_, '_> {
    /// Paint `block` with its top-left corner at (x, y).
    pub fn block(&mut self, canvas: &mut Canvas, block: &Block, x: f32, y: f32, nested: bool) {
        if !canvas.touches(y, block.height) {
            return;
        }
        let radius = if nested {
            (self.geometry.radius * 0.6).round()
        } else {
            self.geometry.radius
        };
        let card = if nested {
            self.theme.nested_card
        } else {
            self.theme.card
        };
        canvas.round_rect(
            Rect {
                x,
                y,
                w: block.width,
                h: block.height,
            },
            radius,
            Corners::ALL,
            card,
        );

        let last = block.rows.len().saturating_sub(1);
        for (index, row) in block.rows.iter().enumerate() {
            if index % 2 == 1 && canvas.touches(y + row.y, row.height) {
                let corners = Corners {
                    top: false,
                    bottom: index == last,
                };
                canvas.round_rect(
                    Rect {
                        x,
                        y: y + row.y,
                        w: block.width,
                        h: row.height,
                    },
                    radius,
                    corners,
                    self.theme.zebra,
                );
            }
        }

        if block.header_height > 0.0 {
            let band = if nested {
                self.theme.zebra
            } else {
                self.theme.header_band
            };
            let corners = Corners {
                top: true,
                bottom: block.rows.is_empty(),
            };
            canvas.round_rect(
                Rect {
                    x,
                    y,
                    w: block.width,
                    h: block.header_height,
                },
                radius,
                corners,
                band,
            );
            let rule = (self.geometry.px / 16.0).max(1.0).round();
            canvas.round_rect(
                Rect {
                    x,
                    y: y + block.header_height - rule,
                    w: block.width,
                    h: rule,
                },
                0.0,
                Corners::NONE,
                self.theme.rule,
            );
            for (column, cell) in block.columns.iter().zip(&block.header) {
                self.cell(canvas, cell, x + column.x, y + self.geometry.pad_y);
            }
        }

        for (row, cells) in block.rows.iter().zip(&block.cells) {
            if !canvas.touches(y + row.y, row.height) {
                continue;
            }
            for (column, cell) in block.columns.iter().zip(cells) {
                self.cell(canvas, cell, x + column.x, y + row.y + self.geometry.pad_y);
            }
        }
    }

    fn cell(&mut self, canvas: &mut Canvas, cell: &Placed, x: f32, y: f32) {
        for (line_index, (dx, line)) in cell.lines.iter().enumerate() {
            let baseline = y + self.geometry.ascent + line_index as f32 * self.geometry.line_height;
            if !canvas.touches(
                baseline - self.geometry.line_height,
                2.0 * self.geometry.line_height,
            ) {
                continue;
            }
            let pad = (self.geometry.px * 0.15).round();
            for highlight in &line.highlights {
                let rect = Rect {
                    x: x + dx + highlight.start - pad,
                    y: baseline - self.geometry.ascent,
                    w: highlight.end - highlight.start + 2.0 * pad,
                    h: self.geometry.line_height,
                };
                canvas.round_rect(rect, pad, Corners::ALL, highlight.color);
            }
            for glyph in &line.glyphs {
                if let Some(image) = self
                    .rasterizer
                    .glyph(glyph.face, glyph.id, self.geometry.px)
                {
                    let color = glyph.color.unwrap_or(cell.color);
                    canvas.glyph(image, x + dx + glyph.x, baseline - glyph.y, color);
                }
            }
        }
        if let Some(grid) = &cell.grid {
            let top = y - self.geometry.pad_y + self.geometry.inset;
            self.block(canvas, grid, x, top, true);
        }
    }
}

/// Paint a string laid out as terminal text, on its own card.
pub fn text(
    canvas: &mut Canvas,
    block: &TextBlock,
    theme: &Theme,
    rasterizer: &mut Rasterizer<'_>,
) {
    let radius = (block.cell.1 * 0.35).round();
    canvas.round_rect(
        Rect {
            x: 0.0,
            y: 0.0,
            w: block.width,
            h: block.height,
        },
        radius,
        Corners::ALL,
        theme.card,
    );
    let (cell_w, cell_h) = block.cell;
    let stroke = (block.px / 14.0).max(1.0).round();
    for (row, cells) in block.lines.iter().enumerate() {
        let y = block.pad.1 + row as f32 * cell_h;
        if !canvas.touches(y - cell_h, 3.0 * cell_h) {
            continue;
        }
        for cell in cells {
            let x = block.pad.0 + cell.column as f32 * cell_w;
            let w = cell.width as f32 * cell_w;
            if let Some(background) = cell.background {
                let rect = Rect { x, y, w, h: cell_h };
                canvas.round_rect(rect, 0.0, Corners::NONE, background);
            }
            let line = match cell.rule {
                Some(Rule::Vertical) => Some(Rect {
                    x: (x + w / 2.0 - stroke / 2.0).round(),
                    y,
                    w: stroke,
                    h: cell_h,
                }),
                Some(Rule::Horizontal) => Some(Rect {
                    x,
                    y: (y + cell_h / 2.0 - stroke / 2.0).round(),
                    w,
                    h: stroke,
                }),
                None => None,
            };
            if let Some(line) = line {
                canvas.round_rect(line, 0.0, Corners::NONE, cell.color);
            }
            for glyph in &cell.glyphs {
                if let Some(image) = rasterizer.glyph(glyph.face, glyph.id, block.px) {
                    canvas.glyph(image, x + glyph.x, y + block.baseline - glyph.y, cell.color);
                }
            }
            if cell.underline {
                let rect = Rect {
                    x,
                    y: (y + block.baseline + block.descent * 0.4).round(),
                    w,
                    h: stroke,
                };
                canvas.round_rect(rect, 0.0, Corners::NONE, cell.color);
            }
        }
    }
}
