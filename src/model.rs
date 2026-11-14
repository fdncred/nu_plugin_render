//! A nushell value as a grid of styled text, before anything is measured.

use std::collections::HashSet;

use nu_ansi_term::{Color, Style};
use nu_protocol::{Config, Record, TableIndexMode, Value};

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
}

/// Turns values into grids.
pub struct Builder<'a> {
    pub config: &'a Config,
    pub theme: &'a Theme,
    pub styles: &'a mut dyn Styles,
    /// Nested lists and records deeper than this show as `[table 3 rows]`.
    pub max_depth: usize,
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
                    rows: vec![vec![self.cell(other, None, depth)]],
                    align: vec![kinds.align()],
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
                            self.cell(cell, Some(column), depth)
                        }
                        None => self.empty(),
                    })
                    .collect(),
                other => {
                    kinds[0].add(other);
                    let mut row = vec![self.cell(other, None, depth)];
                    row.resize_with(width, || self.empty());
                    row
                }
            };
            rows.push(row);
        }

        let show_index = match self.config.table.index_mode {
            TableIndexMode::Always => true,
            TableIndexMode::Never => false,
            TableIndexMode::Auto => has_index_column,
        };
        let index = show_index.then(|| {
            vals.iter()
                .enumerate()
                .map(|(row, value)| {
                    let text = match value {
                        Value::Record { val, .. } => val
                            .get("index")
                            .map(|index| index.to_abbreviated_string(self.config)),
                        _ => None,
                    }
                    .unwrap_or_else(|| row.to_string());
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
        }
    }

    /// A record: one row per field, the key styled like a header.
    fn record(&mut self, record: &Record, depth: usize) -> Grid {
        let rows = record
            .iter()
            .map(|(key, value)| vec![self.header_cell(key), self.cell(value, Some(key), depth)])
            .collect();
        Grid {
            header: None,
            index: None,
            rows,
            align: vec![Align::Left, Align::Left],
        }
    }

    /// One value's cell. Lists and records nest as grids until `max_depth`.
    fn cell(&mut self, value: &Value, column: Option<&str>, depth: usize) -> Cell {
        let nested = match value {
            Value::List { vals, .. } => !vals.is_empty(),
            Value::Record { val, .. } => !val.is_empty(),
            _ => false,
        };
        if nested && depth < self.max_depth {
            return Cell {
                content: Content::Grid {
                    grid: Box::new(self.grid(value, depth + 1)),
                    summary: value.to_abbreviated_string(self.config),
                },
                color: self.theme.text,
                bold: false,
                background: None,
            };
        }

        let text = match value {
            Value::Float { val, .. } if self.config.float_precision >= 0 => {
                format!("{val:.*}", self.config.float_precision as usize)
            }
            Value::Nothing { .. } => String::new(),
            _ => value.to_abbreviated_string(self.config),
        };
        let style = column
            .and_then(|column| match value {
                Value::String { val, .. } => self.styles.path(column, val),
                _ => None,
            })
            .unwrap_or_else(|| self.styles.value(type_key(value), value));
        Cell {
            content: Content::Text(text.replace('\t', "    ")),
            color: self.theme.foreground(&style, self.theme.text),
            bold: style.is_bold,
            background: style
                .background
                .filter(|color| *color != Color::Default)
                .map(|color| self.theme.color(color, self.theme.card)),
        }
    }

    fn header_cell(&self, text: &str) -> Cell {
        let style = self.styles.named("header");
        Cell {
            content: Content::Text(text.to_string()),
            color: self.theme.foreground(&style, self.theme.text),
            bold: true,
            background: None,
        }
    }

    fn index_cell(&self, text: String) -> Cell {
        let style = self.styles.named("row_index");
        Cell {
            content: Content::Text(text),
            color: self.theme.foreground(&style, self.theme.muted),
            bold: false,
            background: None,
        }
    }

    fn empty(&self) -> Cell {
        Cell {
            content: Content::Text(String::new()),
            color: self.theme.muted,
            bold: false,
            background: None,
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
