//! The lines each `table` theme draws, for `render --theme`.

use std::str::FromStr as _;

use nu_protocol::TableMode;

/// The look `render` draws when no theme asks for lines.
pub const CARD: &str = "card";

/// How a line is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stroke {
    Thin,
    Heavy,
    /// Two thin lines.
    Double,
    Dotted,
    /// A row of small hearts, for `with_love`.
    Hearts,
}

/// How the frame's corners look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    Square,
    Round,
    /// Square, in a heavier stroke than the lines, for `reinforced`.
    Heavy,
}

/// Which lines a theme draws, and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Border {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
    /// Between columns.
    pub columns: bool,
    /// Under the header, and over the footer.
    pub header: bool,
    /// Between every two rows.
    pub rows: bool,
    pub stroke: Stroke,
    pub corner: Corner,
    /// Horizontal lines break between columns, as in `restructured`.
    pub gaps: bool,
    /// A Markdown or reStructuredText table, whose header never moves onto
    /// a line, as `table` keeps it.
    pub markup: bool,
}

/// The frame, lines between columns and under the header: `rounded` and
/// most others are this with a few changes.
const FRAMED: Border = Border {
    top: true,
    bottom: true,
    left: true,
    right: true,
    columns: true,
    header: true,
    rows: false,
    stroke: Stroke::Thin,
    corner: Corner::Square,
    gaps: false,
    markup: false,
};

/// No lines at all.
const BARE: Border = Border {
    top: false,
    bottom: false,
    left: false,
    right: false,
    columns: false,
    header: false,
    ..FRAMED
};

impl Border {
    /// The lines `table --theme name` draws, or `None` for `card`, the look
    /// with no lines.
    ///
    /// # Errors
    ///
    /// Fails, listing the names there are, when `name` is no theme.
    pub fn for_theme(name: &str) -> Result<Option<Border>, String> {
        if name.eq_ignore_ascii_case(CARD) {
            return Ok(None);
        }
        let mode = TableMode::from_str(name)
            .map_err(|names| format!("expected '{CARD}', {names}, but found '{name}'"))?;
        Ok(Some(Self::for_mode(mode)))
    }

    /// The lines of a `table` theme, as `table --theme` draws them.
    pub fn for_mode(mode: TableMode) -> Border {
        match mode {
            TableMode::Basic | TableMode::Thin => Border {
                rows: true,
                ..FRAMED
            },
            TableMode::Rounded => Border {
                corner: Corner::Round,
                ..FRAMED
            },
            TableMode::Single => FRAMED,
            TableMode::Double => Border {
                stroke: Stroke::Double,
                ..FRAMED
            },
            TableMode::Heavy => Border {
                stroke: Stroke::Heavy,
                ..FRAMED
            },
            TableMode::Reinforced => Border {
                header: false,
                corner: Corner::Heavy,
                ..FRAMED
            },
            TableMode::AsciiRounded => Border {
                header: false,
                corner: Corner::Round,
                ..FRAMED
            },
            TableMode::BasicCompact => Border {
                header: false,
                ..FRAMED
            },
            TableMode::Dots => Border {
                header: false,
                stroke: Stroke::Dotted,
                ..FRAMED
            },
            TableMode::Compact => Border {
                left: false,
                right: false,
                ..FRAMED
            },
            TableMode::CompactDouble => Border {
                left: false,
                right: false,
                stroke: Stroke::Double,
                ..FRAMED
            },
            TableMode::WithLove => Border {
                left: false,
                right: false,
                stroke: Stroke::Hearts,
                ..FRAMED
            },
            TableMode::Frameless | TableMode::Psql => Border {
                columns: true,
                header: true,
                ..BARE
            },
            TableMode::Light => Border {
                header: true,
                ..BARE
            },
            TableMode::Markdown => Border {
                top: false,
                bottom: false,
                markup: true,
                ..FRAMED
            },
            TableMode::Restructured => Border {
                top: true,
                bottom: true,
                header: true,
                stroke: Stroke::Double,
                gaps: true,
                markup: true,
                ..BARE
            },
            TableMode::None => BARE,
        }
    }

    /// The names `--theme` takes, `card` first, for `--list`.
    pub fn names() -> impl Iterator<Item = &'static str> {
        std::iter::once(CARD).chain(TableMode::NAMES.iter().copied())
    }

    /// Whether the header can sit on a line, with `header_on_separator`:
    /// the top of the frame, or the line under the header when there is no
    /// top.
    pub fn takes_header(&self) -> bool {
        !self.markup && (self.top || self.header)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_theme_has_lines_and_card_has_none() {
        for name in TableMode::NAMES {
            assert!(matches!(Border::for_theme(name), Ok(Some(_))), "{name}");
        }
        assert_eq!(Border::for_theme("card"), Ok(None));
        assert!(Border::for_theme("plaid").is_err());
        assert_eq!(Border::names().next(), Some("card"));
    }

    #[test]
    fn themes_draw_what_table_draws() {
        let rounded = Border::for_theme("default").ok().flatten();
        assert_eq!(rounded.map(|border| border.corner), Some(Corner::Round));
        let light = Border::for_mode(TableMode::Light);
        assert!(light.header && !light.top && !light.columns);
        let markdown = Border::for_mode(TableMode::Markdown);
        assert!(!markdown.takes_header());
    }
}
