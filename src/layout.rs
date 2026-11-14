//! Measuring a grid with real fonts: column widths, row heights, and where
//! every line of text goes. Nothing here draws.

use crate::ansi;
use crate::border::Border;
use crate::fonts::{FaceId, FacePair, Highlight, Line, Shaper};
use crate::model::{Align, Cell, Content, Grid};
use crate::style::{Rgba, Theme};
use crate::text;

/// What a column too narrow for its text does, from
/// `$env.config.table.trim`.
#[derive(Clone, Debug)]
pub enum Trim {
    /// Wrap onto more lines, between words when `keep_words`, and between
    /// characters when a word alone is too wide.
    Wrap { keep_words: bool },
    /// Cut the text short and end it in `suffix`.
    Truncate { suffix: String },
}

/// How `table`'s flags and settings shape a layout.
#[derive(Clone, Debug)]
pub struct Settings {
    pub trim: Trim,
    /// The theme's lines, or `None` for the card look.
    pub border: Option<Border>,
    /// The lines' color, from `color_config.separator`.
    pub line_color: Rgba,
    /// `$env.config.table.padding`: characters left and right of each
    /// column. One is the card look's spacing.
    pub padding: (usize, usize),
    /// `$env.config.table.header_on_separator`: with lines, the header sits
    /// on the top line instead of in a row of its own.
    pub header_on_separator: bool,
}

impl Settings {
    /// `table`'s defaults, with no lines.
    pub fn card(line_color: Rgba) -> Self {
        Settings {
            trim: Trim::Wrap { keep_words: true },
            border: None,
            line_color,
            padding: (1, 1),
            header_on_separator: false,
        }
    }
}

/// Sizes that follow from the font size, in pixels.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub px: f32,
    pub ascent: f32,
    pub line_height: f32,
    /// Space left of each column.
    pub pad_left: f32,
    /// Space right of each column.
    pub pad_right: f32,
    /// Space above and below each row's text.
    pub pad_y: f32,
    /// Space above and below a nested table. Smaller than `pad_y`, so the
    /// nested table's first line sits close to its neighbours' text.
    pub inset: f32,
    pub radius: f32,
}

impl Geometry {
    pub fn new(px: f32, metrics: swash::Metrics) -> Self {
        Geometry {
            px,
            ascent: metrics.ascent.round(),
            line_height: (metrics.ascent + metrics.descent + metrics.leading).ceil(),
            pad_left: (px * 0.6).round(),
            pad_right: (px * 0.6).round(),
            pad_y: (px * 0.3).round(),
            inset: (px * 0.1).round(),
            radius: (px * 0.5).round(),
        }
    }
}

/// A column: where its content starts and how wide it is.
#[derive(Clone, Copy, Debug)]
pub struct Column {
    pub x: f32,
    pub width: f32,
}

/// A row: its top, relative to the block, and its height.
#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub y: f32,
    pub height: f32,
}

/// A measured cell.
pub struct Placed {
    /// Each line, with its offset from the column's left edge.
    pub lines: Vec<(f32, Line)>,
    pub grid: Option<Box<Block>>,
    pub color: Rgba,
}

/// A measured table, ready to paint.
pub struct Block {
    pub width: f32,
    pub height: f32,
    /// 0 when there is no header.
    pub header_height: f32,
    /// Includes the index column first, when there is one.
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
    pub header: Vec<Placed>,
    pub cells: Vec<Vec<Placed>>,
    /// The header again below the last row, `header_height` tall.
    pub footer: bool,
    /// A line under the rows saying how many `--budget` cut, and its top.
    pub note: Option<(f32, Placed)>,
    /// The theme's lines, or `None` for the card look.
    pub border: Option<Border>,
    pub line_color: Rgba,
    /// The header sits on the top line, `header_height` tall with the line
    /// through its middle.
    pub header_on_line: bool,
    /// Fitting cut a column other than the last short, such as a record's
    /// keys. A nested table like that shows its summary instead.
    pub cramped: bool,
}

/// The least room a nested table gets, in ems: about two dozen characters.
const MIN_NESTED_EMS: f32 = 12.0;

/// Lays grids out at one font size.
pub struct Layout<'s, 'f> {
    pub shaper: &'s mut Shaper<'f>,
    pub faces: FacePair,
    pub geometry: Geometry,
    /// For colors written into the text with ANSI escapes.
    pub theme: &'s Theme,
    pub settings: Settings,
}

/// A run of a cell's text in one style, kept so the text can wrap again.
#[derive(Clone)]
struct Piece {
    text: String,
    bold: bool,
    color: Option<Rgba>,
    background: Option<Rgba>,
}

impl Piece {
    fn plain(text: &str, bold: bool) -> Self {
        Piece {
            text: text.to_string(),
            bold,
            color: None,
            background: None,
        }
    }
}

/// A cell before its column width is known.
struct Measured<'g> {
    lines: Vec<Line>,
    grid: Option<Box<Block>>,
    /// A nested table not laid out yet: that waits until its column's share
    /// of the width is known.
    nested: Option<&'g Grid>,
    /// What a nested table shows instead when it doesn't fit, such as
    /// `{record 12 fields}`.
    summary: Option<&'g str>,
    color: Rgba,
    /// For decimal alignment: the width up to the decimal point, per line.
    integer_widths: Vec<f32>,
    center: bool,
    /// The text, line by line, for wrapping it again.
    source: Vec<Vec<Piece>>,
    /// The cell's own background, behind each whole line.
    background: Option<Rgba>,
}

impl Measured<'_> {
    fn width(&self) -> f32 {
        let text = self.lines.iter().map(|line| line.width).fold(0.0, f32::max);
        self.grid.as_ref().map_or(text, |grid| text.max(grid.width))
    }

    fn blank() -> Self {
        Measured {
            lines: Vec::new(),
            grid: None,
            nested: None,
            summary: None,
            color: Rgba([0; 4]),
            integer_widths: Vec::new(),
            center: false,
            source: Vec::new(),
            background: None,
        }
    }
}

/// Column widths once fitted.
struct Fit {
    widths: Vec<f32>,
    /// For decimal columns, the widest integer part.
    integers: Vec<f32>,
    /// Columns made narrower than their content, whose lines get cut.
    shrunk: Vec<bool>,
    total: f32,
}

impl Layout<'_, '_> {
    /// Lay out `grid` within `max_width` pixels.
    ///
    /// When it doesn't fit, the widest text columns are cut short with an
    /// ellipsis, and nested tables too wide for their column show their
    /// summary instead, as `table --expand` does.
    pub fn block(&mut self, grid: &Grid, max_width: f32) -> Block {
        let has_index = grid.index.is_some();
        let mut align: Vec<Align> = Vec::with_capacity(grid.align.len() + 1);
        if has_index {
            align.push(Align::Right);
        }
        align.extend(&grid.align);
        let column_count = align.len();
        let gutter = self.geometry.pad_left + self.geometry.pad_right;
        let chrome = column_count as f32 * gutter;
        let available = max_width - chrome;

        let mut cells: Vec<Vec<Measured<'_>>> = Vec::with_capacity(grid.rows.len());
        for (row_index, row) in grid.rows.iter().enumerate() {
            let mut measured = Vec::with_capacity(column_count);
            if let Some(index) = &grid.index
                && let Some(cell) = index.get(row_index)
            {
                measured.push(self.measure(cell, Align::Right));
            }
            for (cell, &align) in row.iter().zip(&grid.align) {
                measured.push(self.measure(cell, align));
            }
            measured.resize_with(column_count, Measured::blank);
            cells.push(measured);
        }
        let header: Vec<Measured<'_>> = grid
            .header
            .iter()
            .flatten()
            .map(|cell| self.measure(cell, Align::Left))
            .collect();

        self.lay_out_nested(&mut cells, &header, &align, available);
        let fit = loop {
            let fit = self.fit(&cells, &header, &align, available);
            if fit.total <= available {
                break fit;
            }
            // Still too wide: the widest nested table shows its summary.
            let widest = cells
                .iter()
                .enumerate()
                .flat_map(|(row, cells)| {
                    cells.iter().enumerate().filter_map(move |(column, cell)| {
                        cell.grid.as_ref().map(|grid| (row, column, grid.width))
                    })
                })
                .max_by(|a, b| a.2.total_cmp(&b.2));
            let Some((row, column, _)) = widest else {
                break fit;
            };
            self.collapse(&mut cells[row][column]);
        };
        let Fit {
            widths,
            integers,
            shrunk,
            ..
        } = fit;
        let cramped = shrunk
            .split_last()
            .is_some_and(|(_, before)| before.iter().any(|&cut| cut));

        let suffix = match &self.settings.trim {
            Trim::Truncate { suffix } => suffix.as_str(),
            Trim::Wrap { .. } => "…",
        };
        let ellipsis = self.shaper.line(suffix, self.faces.regular, false);
        let bold_ellipsis = self.shaper.line(suffix, self.faces.bold, false);
        let keep_words = match self.settings.trim {
            Trim::Wrap { keep_words } => Some(keep_words),
            Trim::Truncate { .. } => None,
        };

        let mut columns = Vec::with_capacity(column_count);
        let mut x = 0.0;
        for &width in &widths {
            columns.push(Column {
                x: x + self.geometry.pad_left,
                width,
            });
            x += width + gutter;
        }

        let place = |cell: Measured, column: usize, ellipsis: &Line| -> Placed {
            let width = widths[column];
            let lines = cell
                .lines
                .into_iter()
                .zip(
                    cell.integer_widths
                        .iter()
                        .copied()
                        .chain(std::iter::repeat(0.0)),
                )
                .map(|(mut line, integer)| {
                    if shrunk[column] {
                        line.truncate(width, ellipsis);
                    }
                    let dx = match align[column] {
                        _ if cell.center => (width - line.width) / 2.0,
                        Align::Left => 0.0,
                        Align::Right => width - line.width,
                        Align::Decimal => integers[column] - integer,
                    };
                    (dx.max(0.0), line)
                })
                .collect();
            Placed {
                lines,
                grid: cell.grid,
                color: cell.color,
            }
        };

        let border = self.settings.border;
        let header_on_line = self.settings.header_on_separator
            && !header.is_empty()
            && border.is_some_and(|border| border.takes_header());
        let header_height = if header.is_empty() {
            0.0
        } else if header_on_line {
            self.geometry.line_height
        } else {
            self.geometry.line_height + 2.0 * self.geometry.pad_y
        };
        let header = header
            .into_iter()
            .enumerate()
            .map(|(column, cell)| {
                // Headers line up with their column's numbers.
                let mut placed = place(cell, column, &bold_ellipsis);
                if align[column] == Align::Decimal {
                    for (dx, line) in &mut placed.lines {
                        *dx = (widths[column] - line.width).max(0.0);
                    }
                }
                placed
            })
            .collect();

        let mut rows = Vec::with_capacity(cells.len());
        let mut placed_cells = Vec::with_capacity(cells.len());
        let mut y = header_height;
        for mut row in cells {
            // Text in a column fitting cut short wraps instead, when the
            // config says so.
            if let Some(keep_words) = keep_words {
                for (column, cell) in row.iter_mut().enumerate() {
                    if shrunk[column] && !cell.source.is_empty() && cell.width() > widths[column] {
                        let lines = self.wrap(&cell.source, widths[column], keep_words);
                        cell.lines = with_background(lines, cell.background);
                    }
                }
            }
            let text_row = self.geometry.line_height + 2.0 * self.geometry.pad_y;
            let height = row
                .iter()
                .map(|cell| {
                    let text = cell.lines.len().max(1) as f32 * self.geometry.line_height
                        + 2.0 * self.geometry.pad_y;
                    let grid = cell
                        .grid
                        .as_ref()
                        .map_or(0.0, |grid| grid.height + 2.0 * self.geometry.inset);
                    text.max(grid)
                })
                .fold(text_row, f32::max);
            rows.push(Row { y, height });
            y += height;
            placed_cells.push(
                row.into_iter()
                    .enumerate()
                    .map(|(column, cell)| place(cell, column, &ellipsis))
                    .collect(),
            );
        }

        let note = (grid.hidden > 0).then(|| {
            let text = format!(
                "… {} more {}",
                thousands(grid.hidden),
                if grid.hidden == 1 { "row" } else { "rows" }
            );
            let line = self.shaper.line(&text, self.faces.regular, false);
            let top = y;
            y += self.geometry.line_height + 2.0 * self.geometry.pad_y;
            x = x.max(line.width + gutter);
            (
                top,
                Placed {
                    lines: vec![(0.0, line)],
                    grid: None,
                    color: self.theme.muted,
                },
            )
        });
        let footer = grid.footer && header_height > 0.0;
        if footer {
            y += header_height;
        }
        Block {
            width: x,
            height: y,
            header_height,
            columns,
            rows,
            header,
            cells: placed_cells,
            footer,
            note,
            border,
            line_color: self.settings.line_color,
            header_on_line,
            cramped,
        }
    }

    /// Lay `source` out again in lines no wider than `width`.
    fn wrap(&mut self, source: &[Vec<Piece>], width: f32, keep_words: bool) -> Vec<Line> {
        let mut lines = Vec::new();
        for pieces in source {
            let mut line = Line::default();
            // Spaces where a line wrapped are dropped, but not the indent a
            // line starts with.
            let mut wrapped = false;
            for piece in pieces {
                for token in tokens(&piece.text, keep_words) {
                    let blank = token.trim().is_empty();
                    if blank && wrapped {
                        continue;
                    }
                    let shaped = self.piece(token, piece);
                    if line.glyphs.is_empty() || line.width + shaped.width <= width {
                        if shaped.width <= width || blank {
                            line.extend(shaped);
                            wrapped = false;
                            continue;
                        }
                    } else {
                        lines.push(std::mem::take(&mut line));
                        wrapped = true;
                        if blank {
                            continue;
                        }
                        if shaped.width <= width {
                            line.extend(shaped);
                            wrapped = false;
                            continue;
                        }
                    }
                    // A word wider than the column breaks between characters.
                    for cluster in text::clusters(token) {
                        let shaped = self.piece(cluster, piece);
                        if !line.glyphs.is_empty() && line.width + shaped.width > width {
                            lines.push(std::mem::take(&mut line));
                        }
                        line.extend(shaped);
                    }
                    wrapped = false;
                }
            }
            lines.push(line);
        }
        lines
    }

    /// One piece of text shaped in its style.
    fn piece(&mut self, text: &str, piece: &Piece) -> Line {
        let face: FaceId = if piece.bold {
            self.faces.bold
        } else {
            self.faces.regular
        };
        let mut line = Line::default();
        self.shaper
            .append(&mut line, text, face, false, piece.color);
        if let Some(color) = piece.background {
            line.highlights.push(Highlight {
                start: 0.0,
                end: line.width,
                color,
            });
        }
        line
    }

    /// Lay out each column's nested tables within what the other columns
    /// leave: their text, but no more than an even share for text that can
    /// be cut. A nested table that still doesn't fit shows its summary.
    fn lay_out_nested(
        &mut self,
        cells: &mut [Vec<Measured<'_>>],
        header: &[Measured<'_>],
        align: &[Align],
        available: f32,
    ) {
        let count = align.len();
        let mut text = vec![0.0f32; count];
        for row in cells.iter() {
            for (column, cell) in row.iter().enumerate() {
                text[column] = text[column].max(cell.width());
            }
        }
        for (column, cell) in header.iter().enumerate().take(count) {
            text[column] = text[column].max(cell.width());
        }
        let share = available / count.max(1) as f32;
        for column in 0..count {
            if !cells.iter().any(|row| row[column].nested.is_some()) {
                continue;
            }
            let others: f32 = (0..count)
                .filter(|&other| other != column)
                .map(|other| match align[other] {
                    Align::Left => text[other].min(share),
                    _ => text[other],
                })
                .sum();
            let budget = available - others;
            for row in cells.iter_mut() {
                let cell = &mut row[column];
                if let Some(nested) = cell.nested.take() {
                    // With less room than this, a nested table is mostly
                    // ellipses, so its summary says more.
                    if budget < self.geometry.px * MIN_NESTED_EMS {
                        self.collapse(cell);
                        continue;
                    }
                    let block = self.block(nested, budget);
                    if block.width <= budget && !block.cramped {
                        cell.grid = Some(Box::new(block));
                    } else {
                        self.collapse(cell);
                    }
                }
            }
        }
    }

    /// Show a nested table's summary in its place.
    fn collapse(&mut self, cell: &mut Measured<'_>) {
        cell.grid = None;
        cell.nested = None;
        if let Some(summary) = cell.summary {
            cell.lines = vec![self.shaper.line(summary, self.faces.regular, false)];
            cell.source = vec![vec![Piece::plain(summary, false)]];
        }
    }

    /// Fit the columns into `available`: take the overflow from the widest
    /// column that can give, again and again, never below its floor.
    fn fit(
        &self,
        cells: &[Vec<Measured<'_>>],
        header: &[Measured<'_>],
        align: &[Align],
        available: f32,
    ) -> Fit {
        let count = align.len();
        // Natural widths. A decimal column is as wide as its widest integer
        // part plus its widest fraction.
        let mut widths = vec![0.0f32; count];
        let mut integers = vec![0.0f32; count];
        // How narrow fitting may make a column: numbers never shrink, since a
        // cut number reads as a different number, and a column of nested
        // tables stays as wide as its widest one.
        let min_text = self.geometry.px * 4.0;
        let mut floors = vec![min_text; count];
        for row in cells {
            for (column, cell) in row.iter().enumerate() {
                if let Some(grid) = &cell.grid {
                    floors[column] = floors[column].max(grid.width.ceil());
                }
                if align[column] == Align::Decimal {
                    for &integer in &cell.integer_widths {
                        integers[column] = integers[column].max(integer);
                    }
                }
            }
        }
        for row in cells {
            for (column, cell) in row.iter().enumerate() {
                let width = if align[column] == Align::Decimal {
                    cell.lines
                        .iter()
                        .zip(&cell.integer_widths)
                        .map(|(line, integer)| integers[column] + line.width - integer)
                        .fold(0.0, f32::max)
                } else {
                    cell.width()
                };
                widths[column] = widths[column].max(width);
            }
        }
        for (column, cell) in header.iter().enumerate().take(count) {
            widths[column] = widths[column].max(cell.width());
        }
        for (column, width) in widths.iter_mut().enumerate() {
            *width = width.ceil();
            if align[column] != Align::Left {
                floors[column] = *width;
            }
        }

        let mut shrunk = vec![false; count];
        loop {
            let total: f32 = widths.iter().sum();
            if total <= available {
                break;
            }
            let widest = (0..count)
                .filter(|&column| widths[column] > floors[column])
                .max_by(|&a, &b| widths[a].total_cmp(&widths[b]));
            let Some(column) = widest else {
                break;
            };
            widths[column] = (widths[column] - (total - available))
                .max(floors[column])
                .floor();
            shrunk[column] = true;
        }
        Fit {
            total: widths.iter().sum(),
            widths,
            integers,
            shrunk,
        }
    }

    fn measure<'g>(&mut self, cell: &'g Cell, align: Align) -> Measured<'g> {
        let face = if cell.bold {
            self.faces.bold
        } else {
            self.faces.regular
        };
        let numeric = align != Align::Left;
        match &cell.content {
            Content::Text(text) if ansi::has_escapes(text) => {
                // Text that styles itself: each span in its own color, and
                // bold where it asks for it.
                let mut source = Vec::new();
                let lines = ansi::parse(text)
                    .iter()
                    .map(|spans| {
                        let mut line = Line::default();
                        let mut pieces = Vec::with_capacity(spans.len());
                        for span in spans {
                            let colors = span.style.colors(self.theme, cell.color);
                            let face = if cell.bold || span.style.bold {
                                self.faces.bold
                            } else {
                                self.faces.regular
                            };
                            let text = span.text.replace('\t', "    ");
                            let start = line.width;
                            self.shaper
                                .append(&mut line, &text, face, numeric, colors.fg);
                            if let Some(color) = colors.bg {
                                line.highlights.push(Highlight {
                                    start,
                                    end: line.width,
                                    color,
                                });
                            }
                            pieces.push(Piece {
                                text,
                                bold: cell.bold || span.style.bold,
                                color: colors.fg,
                                background: colors.bg,
                            });
                        }
                        source.push(pieces);
                        line
                    })
                    .collect();
                Measured {
                    lines: with_background(lines, cell.background),
                    color: cell.color,
                    center: cell.center,
                    source,
                    background: cell.background,
                    ..Measured::blank()
                }
            }
            Content::Text(text) => {
                let mut integer_widths = Vec::new();
                let source = text
                    .split('\n')
                    .map(|line| vec![Piece::plain(line, cell.bold)])
                    .collect();
                let lines = text
                    .split('\n')
                    .map(|line| {
                        if align == Align::Decimal {
                            let integer = line.rfind('.').map_or(line, |dot| &line[..dot]);
                            integer_widths.push(self.shaper.line(integer, face, true).width);
                        }
                        self.shaper.line(line, face, numeric)
                    })
                    .collect();
                Measured {
                    lines: with_background(lines, cell.background),
                    color: cell.color,
                    integer_widths,
                    center: cell.center,
                    source,
                    background: cell.background,
                    ..Measured::blank()
                }
            }
            Content::Grid { grid, summary } => Measured {
                nested: Some(grid),
                summary: Some(summary),
                color: cell.color,
                ..Measured::blank()
            },
        }
    }
}

/// A count with thousands separators, such as `1,234`.
pub fn thousands(count: usize) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Where text may break: words and the spaces between them, or with
/// `keep_words` off, every character.
fn tokens(text: &str, keep_words: bool) -> Vec<&str> {
    if !keep_words {
        return text::clusters(text).collect();
    }
    let mut tokens = Vec::new();
    let mut start = 0;
    let mut blank = None;
    for (at, c) in text.char_indices() {
        let is_blank = c.is_whitespace();
        if blank.is_some_and(|was| was != is_blank) {
            tokens.push(&text[start..at]);
            start = at;
        }
        blank = Some(is_blank);
    }
    if start < text.len() {
        tokens.push(&text[start..]);
    }
    tokens
}

/// Put a cell's background behind the whole of each of its lines.
fn with_background(mut lines: Vec<Line>, background: Option<Rgba>) -> Vec<Line> {
    if let Some(color) = background {
        for line in &mut lines {
            line.highlights.insert(
                0,
                Highlight {
                    start: 0.0,
                    end: line.width,
                    color,
                },
            );
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_get_separators() {
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(1234), "1,234");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn tokens_split_words_from_spaces() {
        assert_eq!(tokens("a  bc d", true), ["a", "  ", "bc", " ", "d"]);
        assert_eq!(tokens("  indented", true), ["  ", "indented"]);
        assert_eq!(tokens("ab c", false), ["a", "b", " ", "c"]);
    }
}
