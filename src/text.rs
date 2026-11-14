//! A string drawn as terminal text: a monospace grid of cells in the
//! string's ANSI colors. Text that lines itself up with spaces, such as the
//! output of `grid`, stays lined up, because every character sits in the
//! cells `unicode-width` gives it, whatever its glyph's own width.

use std::collections::HashMap;

use unicode_width::UnicodeWidthStr as _;

use crate::ansi;
use crate::fonts::{self, FacePair, Fonts, Glyph, Shaper};
use crate::style::{Rgba, Theme};

/// Tab stops are every 8 cells, as in a terminal.
const TAB: u32 = 8;

/// Text wider than the screen shrinks to fit, but not below this share of
/// the terminal's size. Lines still too long then wrap, as in a terminal.
const MIN_SCALE: f32 = 0.75;

/// A line drawn instead of a box-drawing character, so lines meet from
/// cell to cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    Vertical,
    Horizontal,
}

/// One character, or one emoji sequence, in its cells.
pub struct TextCell {
    pub column: u32,
    /// How many cells it covers: 1, or 2 for wide characters.
    pub width: u32,
    pub glyphs: Vec<Glyph>,
    pub color: Rgba,
    pub background: Option<Rgba>,
    pub underline: bool,
    pub rule: Option<Rule>,
}

/// A laid-out string.
pub struct TextBlock {
    /// The cells of each line that draw something; blank cells are left out.
    pub lines: Vec<Vec<TextCell>>,
    /// One text cell, in image pixels.
    pub cell: (f32, f32),
    /// Space around the text, in image pixels.
    pub pad: (f32, f32),
    pub width: f32,
    pub height: f32,
    pub px: f32,
    /// From a cell's top to the baseline.
    pub baseline: f32,
    pub descent: f32,
}

/// Lay `text` out in `faces` (monospace) on cells the size of the
/// terminal's, shrunk evenly when the text would be wider than `max_width`.
///
/// # Errors
///
/// Fails when the font has no metrics.
pub fn lay_out(
    text: &str,
    fonts: &Fonts,
    faces: FacePair,
    theme: &Theme,
    cell: (f32, f32),
    max_width: f32,
) -> Result<TextBlock, String> {
    let parsed = ansi::parse(text);
    let columns = parsed
        .iter()
        .map(|line| line_width(line))
        .max()
        .unwrap_or(0);

    let pad_cells = (1.0, 0.5);
    let needed = (columns as f32 + 2.0 * pad_cells.0) * cell.0;
    let scale = (max_width / needed).clamp(MIN_SCALE, 1.0);
    let cell = (cell.0 * scale, cell.1 * scale);
    let wrap_at = (max_width / cell.0 - 2.0 * pad_cells.0).floor().max(1.0) as u32;
    let columns = columns.min(wrap_at);

    // The font size whose digits fill a cell's width, or about the
    // terminal's text size, whichever is smaller.
    let probe = Shaper::new(fonts, 100.0)
        .line("0", faces.regular, false)
        .width
        / 100.0;
    let px = (cell.1 * 0.8)
        .min(cell.0 / probe.max(0.01))
        .floor()
        .max(1.0);
    let metrics = fonts::metrics(fonts, faces.regular, px).ok_or("the font has no metrics")?;
    let text_height = metrics.ascent + metrics.descent;
    let baseline = ((cell.1 - text_height) / 2.0 + metrics.ascent).round();

    let mut shaper = Shaper::new(fonts, px);
    let mut shaped: HashMap<(String, bool), Vec<Glyph>> = HashMap::new();
    let mut lines = Vec::with_capacity(parsed.len());
    for spans in &parsed {
        let mut cells = Vec::new();
        let mut column = 0;
        for span in spans {
            let colors = span.style.colors(theme, theme.text);
            let color = colors.fg.unwrap_or(theme.text);
            let face = if span.style.bold {
                faces.bold
            } else {
                faces.regular
            };
            for cluster in clusters(&span.text) {
                if cluster == "\t" {
                    column = (column / TAB + 1) * TAB;
                    if column >= wrap_at {
                        lines.push(std::mem::take(&mut cells));
                        column = 0;
                    }
                    continue;
                }
                let width = cluster.width() as u32;
                if width == 0 {
                    continue;
                }
                if column > 0 && column + width > wrap_at {
                    lines.push(std::mem::take(&mut cells));
                    column = 0;
                }
                let rule = match cluster {
                    "│" => Some(Rule::Vertical),
                    "─" => Some(Rule::Horizontal),
                    _ => None,
                };
                let blank = cluster.trim().is_empty();
                if blank && colors.bg.is_none() && !span.style.underline {
                    column += width;
                    continue;
                }
                let glyphs = if rule.is_some() || blank {
                    Vec::new()
                } else {
                    shaped
                        .entry((cluster.to_string(), span.style.bold))
                        .or_insert_with(|| shaper.line(cluster, face, false).glyphs)
                        .clone()
                };
                cells.push(TextCell {
                    column,
                    width,
                    glyphs,
                    // A plain box-drawing line is a quiet rule, not text.
                    color: match (rule, colors.fg) {
                        (Some(_), None) => theme.rule,
                        _ => color,
                    },
                    background: colors.bg,
                    underline: span.style.underline,
                    rule,
                });
                column += width;
            }
        }
        lines.push(cells);
    }

    let pad = (pad_cells.0 * cell.0, pad_cells.1 * cell.1);
    Ok(TextBlock {
        width: columns as f32 * cell.0 + 2.0 * pad.0,
        height: lines.len() as f32 * cell.1 + 2.0 * pad.1,
        lines,
        cell,
        pad,
        px,
        baseline,
        descent: metrics.descent,
    })
}

/// A line's width in cells, tabs included.
fn line_width(spans: &[ansi::Span]) -> u32 {
    let mut column = 0;
    for span in spans {
        for cluster in clusters(&span.text) {
            if cluster == "\t" {
                column = (column / TAB + 1) * TAB;
            } else {
                column += cluster.width() as u32;
            }
        }
    }
    column
}

/// Split text into what takes cells together: a character with the
/// joiners, selectors, and marks that follow it.
pub(crate) fn clusters(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        let mut chars = rest.char_indices();
        let (_, first) = chars.next()?;
        let mut end = first.len_utf8();
        let mut after_joiner = first == '\u{200D}';
        for (at, c) in chars {
            if !(after_joiner || fonts::joins(c)) {
                break;
            }
            after_joiner = c == '\u{200D}';
            end = at + c.len_utf8();
        }
        let (cluster, tail) = rest.split_at(end);
        rest = tail;
        Some(cluster)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clusters_keep_emoji_sequences_together() {
        let parts: Vec<&str> = clusters("a👨‍👩‍👧b\u{301}c").collect();
        assert_eq!(parts, ["a", "👨‍👩‍👧", "b\u{301}", "c"]);
    }

    #[test]
    fn widths_count_wide_characters_and_tabs() {
        let lines = ansi::parse("日本\tx");
        assert_eq!(line_width(&lines[0]), 9);
    }
}
