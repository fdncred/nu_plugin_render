//! Drawing a measured table into RGBA pixels, one horizontal band at a time,
//! so a long table never needs one huge image.

use swash::scale::image::{Content, Image};

use nu_ansi_term::Color;

use crate::border::{Border, Corner, Stroke};
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

    /// Outline a rectangle with round corners of radius `r`, `weight`
    /// thick, inside its edges.
    pub fn stroke_round_rect(&mut self, rect: Rect, r: f32, weight: f32, color: Rgba) {
        let inner = Rect {
            x: rect.x + weight,
            y: rect.y + weight,
            w: rect.w - 2.0 * weight,
            h: rect.h - 2.0 * weight,
        };
        let inner_r = (r - weight).max(0.0);
        let (x0, x1) = (rect.x.floor() as i32, (rect.x + rect.w).ceil() as i32);
        let (y0, y1) = (rect.y.floor() as i32, (rect.y + rect.h).ceil() as i32);
        // Rows near the top and bottom need every pixel; the rest only the
        // sides.
        let reach = r.max(weight).ceil() as i32 + 1;
        let side = weight.ceil() as i32 + 1;
        for py in y0.max(self.top)..y1.min(self.bottom()) {
            let whole = py < y0 + reach || py >= y1 - reach;
            let mut paint = |px: i32| {
                let coverage = box_coverage(rect, r, px, py) - box_coverage(inner, inner_r, px, py);
                if coverage > 0.0 {
                    self.blend(px, py, color, coverage);
                }
            };
            if whole {
                (x0..x1).for_each(&mut paint);
            } else {
                (x0..x0 + side).for_each(&mut paint);
                (x1 - side..x1).for_each(&mut paint);
            }
        }
    }

    /// A small heart centered on (cx, cy), `size` wide.
    pub fn heart(&mut self, cx: f32, cy: f32, size: f32, color: Rgba) {
        let half = size / 2.0;
        let (x0, y0) = ((cx - half).floor() as i32, (cy - half).floor() as i32);
        let (x1, y1) = ((cx + half).ceil() as i32, (cy + half).ceil() as i32);
        for py in y0.max(self.top)..y1.min(self.bottom()) {
            for px in x0..x1 {
                // 4 × 4 samples of the heart curve (x² + y² − 1)³ − x²y³ ≤ 0.
                let mut inside = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let u = (px as f32 + (sx as f32 + 0.5) / 4.0 - cx) / half * 1.25;
                        let v = -(py as f32 + (sy as f32 + 0.5) / 4.0 - cy) / half * 1.25 + 0.15;
                        let a = u * u + v * v - 1.0;
                        if a * a * a - u * u * v * v * v <= 0.0 {
                            inside += 1;
                        }
                    }
                }
                if inside > 0 {
                    self.blend(px, py, color, inside as f32 / 16.0);
                }
            }
        }
    }

    /// Make a rectangle transparent again.
    pub fn clear(&mut self, rect: Rect) {
        let (x0, x1) = (
            (rect.x.round() as i32).max(0),
            ((rect.x + rect.w).round() as i32).min(self.width as i32),
        );
        let (y0, y1) = (
            (rect.y.round() as i32).max(self.top),
            ((rect.y + rect.h).round() as i32).min(self.bottom()),
        );
        for py in y0..y1 {
            let row = (py - self.top) as usize * self.width as usize;
            for px in x0..x1 {
                let at = (row + px as usize) * 4;
                self.rgba[at..at + 4].fill(0);
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

/// How much of pixel (px, py) lies inside `rect` with round corners of
/// radius `r`, from its signed distance to the rounded box.
fn box_coverage(rect: Rect, r: f32, px: i32, py: i32) -> f32 {
    let (hx, hy) = (rect.w / 2.0, rect.h / 2.0);
    if hx <= 0.0 || hy <= 0.0 {
        return 0.0;
    }
    let r = r.min(hx).min(hy);
    let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
    let qx = (cx - (rect.x + hx)).abs() - (hx - r);
    let qy = (cy - (rect.y + hy)).abs() - (hy - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let distance = outside + qx.max(qy).min(0.0) - r;
    (0.5 - distance).clamp(0.0, 1.0)
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

/// A theme's line style, for drawing its lines.
struct Pen {
    stroke: Stroke,
    /// A thin line's width.
    lw: f32,
    /// The whole stroke's width.
    weight: f32,
    color: Rgba,
}

impl Pen {
    /// A horizontal line centered on `y` along each span.
    fn hline(&self, canvas: &mut Canvas, y: f32, spans: &[(f32, f32)]) {
        for &(from, to) in spans {
            self.line(canvas, (from, y), (to, y));
        }
    }

    /// A vertical line centered on `x`.
    fn vline(&self, canvas: &mut Canvas, x: f32, from: f32, to: f32) {
        self.line(canvas, (x, from), (x, to));
    }

    /// A straight line, across or down, centered on the segment.
    fn line(&self, canvas: &mut Canvas, from: (f32, f32), to: (f32, f32)) {
        let across = from.1 == to.1;
        // A rectangle along the line, `offset` from its center, `thick`.
        let bar = |offset: f32, thick: f32| {
            if across {
                Rect {
                    x: from.0,
                    y: from.1 + offset - thick / 2.0,
                    w: to.0 - from.0,
                    h: thick,
                }
            } else {
                Rect {
                    x: from.0 + offset - thick / 2.0,
                    y: from.1,
                    w: thick,
                    h: to.1 - from.1,
                }
            }
        };
        match self.stroke {
            Stroke::Thin | Stroke::Heavy => {
                canvas.round_rect(bar(0.0, self.weight), 0.0, Corners::NONE, self.color);
            }
            Stroke::Double => {
                for offset in [-self.lw, self.lw] {
                    canvas.round_rect(bar(offset, self.lw), 0.0, Corners::NONE, self.color);
                }
            }
            Stroke::Dotted | Stroke::Hearts => {
                let size = self.weight;
                let step = if self.stroke == Stroke::Dotted {
                    3.0 * self.lw
                } else {
                    2.0 * size
                };
                let length = if across { to.0 - from.0 } else { to.1 - from.1 };
                let mut along = 0.0;
                while along + size <= length + 0.5 {
                    let (cx, cy) = if across {
                        (from.0 + along + size / 2.0, from.1)
                    } else {
                        (from.0, from.1 + along + size / 2.0)
                    };
                    if self.stroke == Stroke::Dotted {
                        let dot = Rect {
                            x: cx - size / 2.0,
                            y: cy - size / 2.0,
                            w: size,
                            h: size,
                        };
                        canvas.round_rect(dot, 0.0, Corners::NONE, self.color);
                    } else {
                        canvas.heart(cx, cy, size, self.color);
                    }
                    along += step;
                }
            }
        }
    }
}

/// Paints blocks onto a canvas.
pub struct Painter<'a, 'f> {
    pub theme: &'a Theme,
    pub geometry: Geometry,
    pub rasterizer: &'a mut Rasterizer<'f>,
}

impl Painter<'_, '_> {
    /// Paint `block` with its top-left corner at (x, y): on a card, or in
    /// its theme's lines.
    pub fn block(&mut self, canvas: &mut Canvas, block: &Block, x: f32, y: f32, nested: bool) {
        if !canvas.touches(y, block.height) {
            return;
        }
        match block.border {
            Some(border) => self.lines(canvas, block, &border, x, y),
            None => self.card(canvas, block, x, y, nested),
        }
        // A header on its line has no padding of its own.
        let text_dy = if block.header_on_line {
            0.0
        } else {
            self.geometry.pad_y
        };
        if block.header_height > 0.0 {
            for (column, cell) in block.columns.iter().zip(&block.header) {
                self.cell(canvas, cell, x + column.x, y + text_dy);
            }
        }
        if block.footer {
            let top = y + block.height - block.header_height;
            for (column, cell) in block.columns.iter().zip(&block.header) {
                self.cell(canvas, cell, x + column.x, top + text_dy);
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
        if let Some((top, note)) = &block.note {
            let left = block.columns.first().map_or(0.0, |column| column.x);
            self.cell(canvas, note, x + left, y + top + self.geometry.pad_y);
        }
    }

    /// The card look: a rounded card, zebra rows, and a header band.
    fn card(&mut self, canvas: &mut Canvas, block: &Block, x: f32, y: f32, nested: bool) {
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
                    bottom: index == last && !block.footer,
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

        if block.header_height <= 0.0 {
            return;
        }
        let band = if nested {
            self.theme.zebra
        } else {
            self.theme.header_band
        };
        let rule = self.line_width();
        let mut band_at = |top: f32, corners: Corners, rule_y: f32| {
            canvas.round_rect(
                Rect {
                    x,
                    y: top,
                    w: block.width,
                    h: block.header_height,
                },
                radius,
                corners,
                band,
            );
            canvas.round_rect(
                Rect {
                    x,
                    y: rule_y,
                    w: block.width,
                    h: rule,
                },
                0.0,
                Corners::NONE,
                self.theme.rule,
            );
        };
        band_at(
            y,
            Corners {
                top: true,
                bottom: block.rows.is_empty(),
            },
            y + block.header_height - rule,
        );
        if block.footer {
            // The header again, rule above it, closing the card.
            let top = y + block.height - block.header_height;
            band_at(
                top,
                Corners {
                    top: false,
                    bottom: true,
                },
                top,
            );
        }
    }

    /// A theme's lines, as `table --theme` draws them in characters.
    fn lines(&mut self, canvas: &mut Canvas, block: &Block, border: &Border, x: f32, y: f32) {
        let lw = self.line_width();
        let weight = match border.stroke {
            Stroke::Heavy => 2.0 * lw,
            Stroke::Double => 3.0 * lw,
            Stroke::Hearts => (self.geometry.px * 0.3).round(),
            Stroke::Thin | Stroke::Dotted => lw,
        };
        let color = match border.stroke {
            Stroke::Hearts => self.theme.color(Color::Red, block.line_color),
            _ => block.line_color,
        };
        let pen = Pen {
            stroke: border.stroke,
            lw,
            weight,
            color,
        };
        let (left, right, bottom) = (x, x + block.width, y + block.height);
        // The topmost line: the frame's top, or with the header on it, a
        // line through the header's middle.
        let top_line = (border.top || block.header_on_line).then(|| {
            if block.header_on_line {
                y + block.header_height / 2.0
            } else {
                y + weight / 2.0
            }
        });
        let bottom_line = border.bottom.then(|| bottom - weight / 2.0);

        // Where horizontal lines run: across, or broken between columns.
        let gap = 2.0 * lw;
        let spans: Vec<(f32, f32)> = if border.gaps {
            block
                .columns
                .iter()
                .map(|column| {
                    (
                        x + column.x - self.geometry.pad_left + gap,
                        x + column.x + column.width + self.geometry.pad_right - gap,
                    )
                })
                .collect()
        } else {
            vec![(left, right)]
        };
        let round = border.corner == Corner::Round
            && border.left
            && border.right
            && border.top
            && border.bottom;

        if round && let (Some(top), Some(_)) = (top_line, bottom_line) {
            let outline = Rect {
                x: left,
                y: top - weight / 2.0,
                w: block.width,
                h: bottom - (top - weight / 2.0),
            };
            canvas.stroke_round_rect(outline, self.geometry.radius, weight, color);
        } else {
            if let Some(top) = top_line {
                pen.hline(canvas, top, &spans);
            }
            if let Some(bottom) = bottom_line {
                pen.hline(canvas, bottom, &spans);
            }
        }
        if border.header && block.header_height > 0.0 && !block.header_on_line {
            pen.hline(canvas, y + block.header_height, &spans);
        }
        if border.header && block.footer {
            pen.hline(canvas, bottom - block.header_height, &spans);
        }
        if border.rows {
            for row in block.rows.iter().skip(1) {
                pen.hline(canvas, y + row.y, &spans);
            }
        }

        let v_top = top_line.unwrap_or(y);
        let v_bottom = bottom_line.unwrap_or(bottom);
        if border.columns {
            // The note under the rows spans the columns, so their lines stop
            // above it, and start again for a footer.
            let spans = match &block.note {
                Some((top, _)) => {
                    let note_end = if block.footer {
                        bottom - block.header_height
                    } else {
                        v_bottom
                    };
                    vec![(v_top, y + top), (note_end, v_bottom)]
                }
                None => vec![(v_top, v_bottom)],
            };
            for pair in block.columns.windows(2) {
                let at = x + pair[0].x + pair[0].width + self.geometry.pad_right;
                for &(from, to) in &spans {
                    if to > from {
                        pen.vline(canvas, at, from, to);
                    }
                }
            }
        }
        if let (true, Some((top, _))) = (border.rows || border.header, &block.note) {
            pen.hline(canvas, y + top, &spans);
        }
        if !round {
            if border.left {
                pen.vline(canvas, left + weight / 2.0, v_top, v_bottom);
            }
            if border.right {
                pen.vline(canvas, right - weight / 2.0, v_top, v_bottom);
            }
        }
        if border.corner == Corner::Heavy
            && let (Some(top), Some(bottom)) = (top_line, bottom_line)
        {
            // `reinforced`: heavier corner pieces over thin lines.
            let arm = self.geometry.pad_left.max(3.0 * lw);
            let thick = 2.0 * lw;
            for (cx, cy, dx, dy) in [
                (left, top - lw / 2.0, 1.0, 1.0),
                (right, top - lw / 2.0, -1.0, 1.0),
                (left, bottom + lw / 2.0, 1.0, -1.0),
                (right, bottom + lw / 2.0, -1.0, -1.0),
            ] {
                let x0 = if dx > 0.0 { cx } else { cx - arm };
                let y0 = if dy > 0.0 { cy } else { cy - thick };
                canvas.round_rect(
                    Rect {
                        x: x0,
                        y: y0,
                        w: arm,
                        h: thick,
                    },
                    0.0,
                    Corners::NONE,
                    color,
                );
                let x0 = if dx > 0.0 { cx } else { cx - thick };
                let y0 = if dy > 0.0 { cy } else { cy - arm };
                canvas.round_rect(
                    Rect {
                        x: x0,
                        y: y0,
                        w: thick,
                        h: arm,
                    },
                    0.0,
                    Corners::NONE,
                    color,
                );
            }
        }
        if let (true, Some(top)) = (block.header_on_line, top_line) {
            // Clear the line around each label, so it sits in a gap.
            let pad = (self.geometry.px * 0.25).round();
            for (column, cell) in block.columns.iter().zip(&block.header) {
                let width = cell
                    .lines
                    .iter()
                    .map(|(dx, line)| dx + line.width)
                    .fold(0.0, f32::max);
                if width > 0.0 {
                    canvas.clear(Rect {
                        x: x + column.x - pad,
                        y: top - weight,
                        w: width + 2.0 * pad,
                        h: 2.0 * weight,
                    });
                }
            }
        }
    }

    /// One thin line's width: about a sixteenth of the font size.
    fn line_width(&self) -> f32 {
        (self.geometry.px / 16.0).max(1.0).round()
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
