//! A nushell value as a grid of styled text, before anything is measured.

use std::collections::HashSet;

use nu_ansi_term::{Color, Style};
use nu_protocol::{Config, Record, Span, TableIndexMode, Value};

use crate::style::{Rgba, Theme};

/// How a column lines its cells up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
    /// Right-aligned on the decimal point, for columns of floats.
    Decimal,
}

/// One cell: text, or a whole table nested inside it.
pub struct Cell {
    pub content: Content,
    pub color: Rgba,
    pub bold: bool,
    /// A color behind the text, such as `LS_COLORS` gives some files.
    pub background: Option<Rgba>,
    /// Centered in its column whatever the column's alignment, as `table`
    /// centers `missing_value_symbol`.
    pub center: bool,
}

pub enum Content {
    Text(String),
    /// A nested table, and the summary it shows when there's no room for
    /// it, such as `{record 12 fields}`.
    Grid {
        grid: Box<Grid>,
        summary: String,
    },
}

/// A table: optional header and index, then rows of cells.
pub struct Grid {
    pub header: Option<Vec<Cell>>,
    pub index: Option<Vec<Cell>>,
    pub rows: Vec<Vec<Cell>>,
    /// One per column, not counting the index.
    pub align: Vec<Align>,
    /// The header again below the last row, per `footer_mode`.
    pub footer: bool,
    /// Rows `--budget` cut off the end, noted under the last one.
    pub hidden: usize,
}

impl Grid {
    /// Its rows, and with `nested`, the rows of the tables inside it too.
    pub fn count_rows(&self, nested: bool) -> usize {
        let inner: usize = if nested {
            self.rows
                .iter()
                .flatten()
                .filter_map(|cell| match &cell.content {
                    Content::Grid { grid, .. } => Some(grid.count_rows(true)),
                    Content::Text(_) => None,
                })
                .sum()
        } else {
            0
        };
        self.rows.len() + inner
    }

    /// Keep the first `keep` rows, counting the rest as hidden, for
    /// `--budget`.
    pub fn cut(&mut self, keep: usize) {
        if keep >= self.rows.len() {
            return;
        }
        self.hidden += self.rows.len() - keep;
        self.rows.truncate(keep);
        if let Some(index) = &mut self.index {
            index.truncate(keep);
        }
    }

    /// What `table` shows for an empty list or record, such as `empty list`,
    /// when `show_empty` is on.
    pub fn placeholder(text: String, theme: &Theme) -> Grid {
        Grid {
            header: None,
            index: None,
            rows: vec![vec![Cell {
                content: Content::Text(text),
                color: theme.muted,
                bold: false,
                background: None,
                center: false,
            }]],
            align: vec![Align::Left],
            footer: false,
            hidden: 0,
        }
    }
}

/// Where cell styles come from: `color_config`, its closures, and
/// `LS_COLORS` for path columns. The command implements it with engine calls.
pub trait Styles {
    /// The style `color_config` gives `value`, whose type is `key`.
    fn value(&mut self, key: &str, value: &Value) -> Style;
    /// A fixed `color_config` entry, such as `header` or `row_index`.
    fn named(&self, key: &str) -> Style;
    /// The `LS_COLORS` style for `text` in `column`, when it is a path column.
    fn path(&mut self, column: &str, text: &str) -> Option<Style>;
    /// `--icons`: the icon for the file or directory at `path`, and its
    /// color.
    fn icon(&self, path: &str) -> (char, Style);
}

/// Turns values into grids.
pub struct Builder<'a> {
    pub config: &'a Config,
    pub theme: &'a Theme,
    pub styles: &'a mut dyn Styles,
    /// How many levels of lists and records nest as tables below the top
    /// one. With 0 they show as summaries such as `[table 3 rows]`, as
    /// `table` shows them; with `usize::MAX` they all nest, as with
    /// `table --expand`.
    pub max_depth: usize,
    /// `--flatten`: in a table's rows, a list of plain values shows as one
    /// line, joined with this separator.
    pub flatten: Option<&'a str>,
    /// `--index`: where row numbers start, or `None` to hide the index
    /// column, as `--index false` does.
    pub index: Option<usize>,
    /// `--icons`: a file icon before each path that `LS_COLORS` styles.
    pub icons: bool,
}

/// Counts of a column's value kinds, to pick its alignment.
#[derive(Default, Clone, Copy)]
struct Kinds {
    numbers: usize,
    floats: usize,
    other: usize,
}

impl Kinds {
    fn add(&mut self, value: &Value) {
        match value {
            Value::Float { .. } => {
                self.numbers += 1;
                self.floats += 1;
            }
            Value::Int { .. } | Value::Filesize { .. } | Value::Duration { .. } => {
                self.numbers += 1;
            }
            Value::Nothing { .. } => {}
            _ => self.other += 1,
        }
    }

    fn align(self) -> Align {
        if self.other > 0 || self.numbers == 0 {
            Align::Left
        } else if self.floats == self.numbers {
            Align::Decimal
        } else {
            Align::Right
        }
    }
}

impl Builder<'_> {
    /// The grid for any value. A list or record is a table; anything else is
    /// one cell.
    pub fn grid(&mut self, value: &Value, depth: usize) -> Grid {
        match value {
            Value::List { vals, .. } => self.list(vals, depth),
            Value::Record { val, .. } => self.record(val, depth),
            other => {
                let mut kinds = Kinds::default();
                kinds.add(other);
                Grid {
                    header: None,
                    index: None,
                    rows: vec![vec![self.cell(other, None, depth, false)]],
                    align: vec![kinds.align()],
                    footer: false,
                    hidden: 0,
                }
            }
        }
    }

    /// A list: its records' columns, in the order they first appear. A list
    /// of plain values is one column with no header.
    fn list(&mut self, vals: &[Value], depth: usize) -> Grid {
        let mut columns: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        let mut has_index_column = false;
        for value in vals {
            if let Value::Record { val, .. } = value {
                for key in val.columns() {
                    if key == "index" {
                        has_index_column = true;
                    } else if seen.insert(key.as_str()) {
                        columns.push(key.clone());
                    }
                }
            }
        }

        let width = columns.len().max(1);
        let mut kinds = vec![Kinds::default(); width];
        let mut rows = Vec::with_capacity(vals.len());
        for value in vals {
            let row = match value {
                Value::Record { val, .. } if !columns.is_empty() => columns
                    .iter()
                    .zip(kinds.iter_mut())
                    .map(|(column, kind)| match val.get(column) {
                        Some(cell) => {
                            kind.add(cell);
                            self.cell(cell, Some(column), depth, true)
                        }
                        None => self.missing(),
                    })
                    .collect(),
                other => {
                    kinds[0].add(other);
                    let mut row = vec![self.cell(other, None, depth, true)];
                    row.resize_with(width, || self.empty());
                    row
                }
            };
            rows.push(row);
        }

        let show_index = self.index.is_some()
            && match self.config.table.index_mode {
                TableIndexMode::Always => true,
                TableIndexMode::Never => false,
                TableIndexMode::Auto => has_index_column,
            };
        // Nested tables count from 0, as in `table`.
        let offset = if depth == 0 {
            self.index.unwrap_or(0)
        } else {
            0
        };
        let index = show_index.then(|| {
            vals.iter()
                .enumerate()
                .map(|(row, value)| {
                    let text = match value {
                        Value::Record { val, .. } => val
                            .get("index")
                            .map(|index| index.to_expanded_string("", self.config)),
                        _ => None,
                    }
                    .unwrap_or_else(|| (row + offset).to_string());
                    self.index_cell(text)
                })
                .collect()
        });

        let header = (!columns.is_empty()).then(|| {
            let mut header: Vec<Cell> = columns.iter().map(|c| self.header_cell(c)).collect();
            if show_index {
                header.insert(0, self.header_cell("#"));
            }
            header
        });

        Grid {
            header,
            index,
            rows,
            align: kinds.into_iter().map(Kinds::align).collect(),
            footer: false,
            hidden: 0,
        }
    }

    /// A record: one row per field, the key styled like a header.
    fn record(&mut self, record: &Record, depth: usize) -> Grid {
        let rows = record
            .iter()
            .map(|(key, value)| {
                vec![
                    self.header_cell(key),
                    self.cell(value, Some(key), depth, false),
                ]
            })
            .collect();
        Grid {
            header: None,
            index: None,
            rows,
            align: vec![Align::Left, Align::Left],
            footer: false,
            hidden: 0,
        }
    }

    /// One value's cell. Lists and records nest as grids until `max_depth`.
    /// A `list_item` is a cell in a list's rows, not a record's value; only
    /// those flatten, as in `table`.
    fn cell(&mut self, value: &Value, column: Option<&str>, depth: usize, list_item: bool) -> Cell {
        let nested = match value {
            Value::List { vals, .. } => !vals.is_empty(),
            Value::Record { val, .. } => !val.is_empty(),
            _ => false,
        };
        if nested && depth < self.max_depth {
            if list_item
                && let (Some(separator), Value::List { vals, .. }) = (self.flatten, value)
                && vals
                    .iter()
                    .all(|val| !matches!(val, Value::Record { .. } | Value::List { .. }))
            {
                let text: Vec<String> = vals.iter().map(|val| self.text(val)).collect();
                return Cell {
                    content: Content::Text(text.join(separator)),
                    color: self.theme.text,
                    bold: false,
                    background: None,
                    center: false,
                };
            }
            return Cell {
                content: Content::Grid {
                    grid: Box::new(self.grid(value, depth + 1)),
                    summary: value.to_abbreviated_string(self.config),
                },
                color: self.theme.text,
                bold: false,
                background: None,
                center: false,
            };
        }

        let mut text = self.text(value).replace('\t', "    ");
        let path_style = column.and_then(|column| match value {
            Value::String { val, .. } => self.styles.path(column, val),
            _ => None,
        });
        let style = path_style.unwrap_or_else(|| self.styles.value(type_key(value), value));
        let mut background = style
            .background
            .filter(|color| *color != Color::Default)
            .map(|color| self.theme.color(color, self.theme.card));
        // As `table --icons`: the icon in its own color, two spaces, then the
        // name in its style, so a background stays behind the name alone.
        if self.icons
            && let Some(style) = path_style
            && let Value::String { val, .. } = value
        {
            let (icon, icon_style) = self.styles.icon(val);
            text = format!(
                "{}  {}",
                icon_style.paint(icon.to_string()),
                style.paint(&text)
            );
            background = None;
        }
        Cell {
            content: Content::Text(text),
            color: self.theme.foreground(&style, self.theme.text),
            bold: style.is_bold,
            background,
            center: false,
        }
    }

    /// A plain value's text: floats at `float_precision`, nothing empty.
    fn text(&self, value: &Value) -> String {
        match value {
            Value::Float { val, .. } if self.config.float_precision >= 0 => {
                format!("{val:.*}", self.config.float_precision as usize)
            }
            Value::Nothing { .. } => String::new(),
            _ => value.to_abbreviated_string(self.config),
        }
    }

    /// A record in a list that lacks one of the list's columns: the
    /// `missing_value_symbol`, centered, in the `empty` color.
    fn missing(&mut self) -> Cell {
        let style = self.styles.value("empty", &Value::nothing(Span::unknown()));
        Cell {
            content: Content::Text(self.config.table.missing_value_symbol.clone()),
            color: self.theme.foreground(&style, self.theme.text),
            bold: style.is_bold,
            background: None,
            center: true,
        }
    }

    fn header_cell(&self, text: &str) -> Cell {
        let style = self.styles.named("header");
        Cell {
            content: Content::Text(text.to_string()),
            color: self.theme.foreground(&style, self.theme.text),
            bold: true,
            background: None,
            center: false,
        }
    }

    fn index_cell(&self, text: String) -> Cell {
        let style = self.styles.named("row_index");
        Cell {
            content: Content::Text(text),
            color: self.theme.foreground(&style, self.theme.muted),
            bold: false,
            background: None,
            center: false,
        }
    }

    fn empty(&self) -> Cell {
        Cell {
            content: Content::Text(String::new()),
            color: self.theme.muted,
            bold: false,
            background: None,
            center: false,
        }
    }
}

/// The `color_config` key for a value's type.
fn type_key(value: &Value) -> &'static str {
    match value {
        Value::Bool { .. } => "bool",
        Value::Int { .. } => "int",
        Value::Float { .. } => "float",
        Value::Filesize { .. } => "filesize",
        Value::Duration { .. } => "duration",
        Value::Date { .. } => "date",
        Value::Range { .. } => "range",
        Value::String { .. } => "string",
        Value::Glob { .. } => "glob",
        Value::Record { .. } => "record",
        Value::List { .. } => "list",
        Value::Closure { .. } => "closure",
        Value::Nothing { .. } => "nothing",
        Value::Binary { .. } => "binary",
        Value::CellPath { .. } => "cell-path",
        Value::Error { .. } => "error",
        Value::Custom { .. } => "custom",
    }
}

#[cfg(test)]
mod tests {
    use nu_protocol::record;

    use super::*;

    /// No colors from anywhere.
    struct Plain;

    impl Styles for Plain {
        fn value(&mut self, _: &str, _: &Value) -> Style {
            Style::default()
        }
        fn named(&self, _: &str) -> Style {
            Style::default()
        }
        fn path(&mut self, _: &str, _: &str) -> Option<Style> {
            None
        }
        fn icon(&self, _: &str) -> (char, Style) {
            ('x', Style::default())
        }
    }

    fn build(value: &Value, max_depth: usize, flatten: Option<&str>, index: Option<usize>) -> Grid {
        let config = Config::default();
        let theme = Theme::dark();
        Builder {
            config: &config,
            theme: &theme,
            styles: &mut Plain,
            max_depth,
            flatten,
            index,
            icons: false,
        }
        .grid(value, 0)
    }

    fn text(cell: &Cell) -> &str {
        match &cell.content {
            Content::Text(text) => text,
            Content::Grid { .. } => panic!("expected text, found a nested table"),
        }
    }

    fn rows() -> Value {
        Value::test_list(vec![
            Value::test_record(record! {
                "a" => Value::test_int(1),
                "b" => Value::test_list(vec![Value::test_int(1), Value::test_int(2)]),
            }),
            Value::test_record(record! { "a" => Value::test_int(2) }),
        ])
    }

    #[test]
    fn nested_values_show_summaries_unless_expanded() {
        let grid = build(&rows(), 0, None, Some(0));
        assert_eq!(text(&grid.rows[0][1]), "[list 2 items]");
        let grid = build(&rows(), usize::MAX, None, Some(0));
        assert!(matches!(grid.rows[0][1].content, Content::Grid { .. }));
    }

    #[test]
    fn missing_columns_show_the_symbol_centered() {
        let grid = build(&rows(), 0, None, Some(0));
        let missing = &grid.rows[1][1];
        assert_eq!(text(missing), Config::default().table.missing_value_symbol);
        assert!(missing.center);
    }

    #[test]
    fn flatten_joins_plain_lists_in_rows_only() {
        let grid = build(&rows(), usize::MAX, Some(", "), Some(0));
        assert_eq!(text(&grid.rows[0][1]), "1, 2");
        let record = Value::test_record(record! {
            "b" => Value::test_list(vec![Value::test_int(1), Value::test_int(2)]),
        });
        let grid = build(&record, usize::MAX, Some(", "), Some(0));
        assert!(matches!(grid.rows[0][1].content, Content::Grid { .. }));
    }

    #[test]
    fn cutting_counts_the_hidden_rows() {
        let mut grid = build(&rows(), 0, None, Some(0));
        grid.cut(1);
        grid.cut(0);
        assert_eq!((grid.rows.len(), grid.hidden), (0, 2));
        assert_eq!(grid.index.map(|index| index.len()), Some(0));
    }

    #[test]
    fn index_counts_from_the_flag_or_hides() {
        let grid = build(&rows(), 0, None, Some(100));
        let index: Vec<&str> = grid.index.iter().flatten().map(text).collect();
        assert_eq!(index, ["100", "101"]);
        assert!(build(&rows(), 0, None, None).index.is_none());
    }
}
