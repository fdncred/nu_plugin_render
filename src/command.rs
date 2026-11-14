//! The `render` command.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lscolors::LsColors;
use nu_ansi_term::Style;
use nu_color_config::{color_record_to_nustyle, lookup_ansi_color_style};
use nu_plugin::{EngineInterface, EvaluatedCall, PluginCommand};
use nu_protocol::engine::Closure;
use nu_protocol::{
    Category, Config, FooterMode, IntoPipelineData as _, LabeledError, PipelineData,
    PipelineMetadata, Record, Signature, Spanned, SyntaxShape, TrimStrategy, Type, Value,
};

use crate::RenderPlugin;
use crate::border::Border;
use crate::draw::{self, Drawing, Screen};
use crate::fonts::Rasterizer;
use crate::kitty;
use crate::layout::{Settings, Trim, thousands};
use crate::model::{Builder, Grid, Styles};
use crate::sixel;
use crate::style::Theme;
use crate::term;

/// Cell rows per image band. Every image costs the terminal time, so fewer,
/// taller bands show sooner: in Rex, `$env | render -e` at 143 columns took
/// 141 ms to show in 22-row bands, 240 ms in 4-row bands, and 318 ms in
/// 1-row bands.
const BAND_ROWS: u32 = 40;

pub struct Render;

impl PluginCommand for Render {
    type Plugin = RenderPlugin;

    fn name(&self) -> &str {
        "render"
    }

    fn description(&self) -> &str {
        "Draw a table as an image, with the kitty graphics protocol."
    }

    fn extra_description(&self) -> &str {
        "Prints what `table` prints when the terminal can't show kitty graphics."
    }

    fn signature(&self) -> Signature {
        Signature::build(self.name())
            .input_output_types(vec![(Type::Any, Type::Any)])
            .switch(
                "png",
                "Return the drawing as PNG bytes instead of drawing it.",
                None,
            )
            .switch(
                "copy",
                "Copy the drawing to the clipboard as an image instead of drawing it.",
                None,
            )
            .switch("mono", "Draw a table in a monospace font.", None)
            .named(
                "width",
                SyntaxShape::Int,
                "The widest the drawing may be, in terminal columns. Defaults to the terminal's width, or 160 with --png or --copy.",
                Some('w'),
            )
            .named(
                "index",
                SyntaxShape::Any,
                "Enable (true) or disable (false) the #/index column or set the starting index.",
                Some('i'),
            )
            .switch(
                "expand",
                "Expand the table structure: nested lists and records draw as tables.",
                Some('e'),
            )
            .named(
                "expand-deep",
                SyntaxShape::Int,
                "How many levels of nesting to expand, with --expand.",
                Some('d'),
            )
            .switch("flatten", "Flatten simple arrays, with --expand.", None)
            .switch(
                "collapse",
                "Expand the table structure in collapse mode: nested tables merge into their cells.",
                Some('c'),
            )
            .named(
                "flatten-separator",
                SyntaxShape::String,
                "Sets a separator when 'flatten' is used.",
                None,
            )
            .named(
                "abbreviated",
                SyntaxShape::Int,
                "Abbreviate the data in the table by truncating the middle part and only showing amount provided on top and bottom.",
                Some('a'),
            )
            .switch("icons", "Add icons to file paths in tables.", Some('o'))
            .named(
                "theme",
                SyntaxShape::String,
                "Draw the lines of a `table` theme, such as `rounded` or `heavy`, or `card`, the default, for none.",
                Some('t'),
            )
            .switch("list", "List the available themes.", Some('l'))
            .named(
                "budget",
                SyntaxShape::Int,
                "Draw at most this many million pixels: a table stops at a whole row and a string at a whole line, with a note of how many more there are.",
                None,
            )
            .category(Category::Viewers)
    }

    fn run(
        &self,
        plugin: &RenderPlugin,
        engine: &EngineInterface,
        call: &EvaluatedCall,
        input: PipelineData,
    ) -> Result<PipelineData, LabeledError> {
        let head = call.head;
        if call.has_flag("list")? {
            let names = Border::names().map(|name| Value::string(name, head));
            return Ok(Value::list(names.collect(), head).into_pipeline_data());
        }
        let border = match call.get_flag::<Spanned<String>>("theme")? {
            Some(name) => Border::for_theme(&name.item).map_err(|err| {
                LabeledError::new("`--theme` isn't a theme").with_label(err, name.span)
            })?,
            None => None,
        };
        let as_png = call.has_flag("png")?;
        let copy = call.has_flag("copy")?;
        let mono = call.has_flag("mono")?;
        let width: Option<Spanned<i64>> = call.get_flag("width")?;
        let width = match width {
            Some(Spanned { item, span }) => {
                Some(u32::try_from(item).ok().filter(|w| *w > 0).ok_or_else(|| {
                    LabeledError::new("`--width` must be a positive number of columns")
                        .with_label("not a positive number", span)
                })?)
            }
            None => None,
        };
        let budget = match call.get_flag::<Spanned<i64>>("budget")? {
            Some(Spanned { item, span }) => Some(
                u64::try_from(item)
                    .ok()
                    .filter(|millions| *millions > 0)
                    .ok_or_else(|| {
                        LabeledError::new("`--budget` must be a positive number of million pixels")
                            .with_label("not a positive number", span)
                    })?
                    .saturating_mul(1_000_000),
            ),
            None => None,
        };
        let collapse = call.has_flag("collapse")?;
        // As in `table`, a collapsed table shows its index only when asked.
        let index_explicit = call
            .get_flag_value("index")
            .is_some_and(|value| !matches!(value, Value::Nothing { .. }));
        let index = index_flag(call)?.filter(|_| !collapse || index_explicit);
        let expand = call.has_flag("expand")?;
        let expand_deep: Option<usize> = call.get_flag("expand-deep")?;
        let flatten = call.has_flag("flatten")?;
        let flatten_separator: Option<String> = call.get_flag("flatten-separator")?;
        let abbreviated: Option<usize> = call.get_flag("abbreviated")?;
        let icons = call.has_flag("icons")?;
        let metadata = input.metadata_ref().cloned();
        let value = input.into_value(head)?;
        let mut timer = Timer::new();

        let caps = if as_png || copy {
            None
        } else {
            let known = plugin.caps.lock().ok().and_then(|caps| caps.clone());
            if known.is_some() {
                debug("reusing the terminal's answers from the first probe");
            }
            let probed = term::probe(engine, known.as_ref());
            if let (None, Ok(caps)) = (&known, &probed)
                && let Ok(mut slot) = plugin.caps.lock()
            {
                *slot = Some(caps.clone());
            }
            match probed {
                // Sixel needs the cell's size in pixels, to know how big to
                // draw; kitty graphics fit the image to cells either way.
                Ok(caps) if caps.kitty || (caps.sixel && caps.cell.is_some()) => Some(caps),
                Ok(caps) => {
                    debug(if caps.sixel {
                        "falling back to `table`: the terminal has sixel but didn't say how big its cells are"
                    } else {
                        "falling back to `table`: the terminal has neither kitty graphics nor sixel"
                    });
                    return fallback(engine, call, value, metadata);
                }
                Err(reason) => {
                    debug(&format!("falling back to `table`: {reason}"));
                    return fallback(engine, call, value, metadata);
                }
            }
        };

        timer.lap("probe");
        let fonts = plugin.fonts().map_err(LabeledError::new)?;
        timer.lap("fonts");
        let theme = Theme::for_background(caps.as_ref().and_then(|caps| caps.background));
        let mut screen = caps.as_ref().map_or(Screen::OFFLINE, |caps| {
            let cell = caps.cell.unwrap_or(Screen::OFFLINE.cell);
            Screen {
                cell,
                columns: caps.columns.max(1),
                rows: caps.rows.max(1),
                scale: Screen::scale_for(cell, caps.kitty),
            }
        });
        if let Some(width) = width {
            screen.columns = width;
        }

        let drawing = match &value {
            // A string, such as `grid`'s output, may line itself up with
            // spaces, so it keeps to terminal cells in a monospace font.
            Value::String { val, .. } => {
                let hooked = display_hook(engine, &value, metadata.as_ref());
                let text = hooked.as_deref().unwrap_or(val);
                let lines: Vec<&str> = text.split('\n').collect();
                within_budget(budget, lines.len(), |keep| {
                    if keep == lines.len() {
                        return draw::lay_out_text(text, &fonts, fonts.mono, &theme, &screen);
                    }
                    let hidden = lines.len() - keep;
                    let cut = format!(
                        "{}\n\x1b[2m… {} more {}\x1b[0m",
                        lines[..keep].join("\n"),
                        thousands(hidden),
                        if hidden == 1 { "line" } else { "lines" }
                    );
                    draw::lay_out_text(&cut, &fonts, fonts.mono, &theme, &screen)
                })
            }
            _ => {
                let config = engine.get_config()?;
                // An empty list or record shows what `table` shows: a
                // dimmed `empty list` with `show_empty`, nothing without.
                let empty = match &value {
                    Value::List { vals, .. } if vals.is_empty() => Some("list"),
                    Value::Record { val, .. } if val.is_empty() => Some("record"),
                    _ => None,
                };
                if empty.is_some() && !config.table.show_empty {
                    return Ok(Value::string("", head).into_pipeline_data());
                }
                let abbreviated = abbreviated
                    .or(config.table.abbreviated_row_count)
                    .and_then(|keep| abbreviate(&value, keep));
                let mut styles = EngineStyles::new(engine, &config, metadata.as_ref());
                let mut grid = Builder {
                    config: &config,
                    theme: &theme,
                    styles: &mut styles,
                    max_depth: if collapse {
                        usize::MAX
                    } else if expand {
                        expand_deep.unwrap_or(usize::MAX)
                    } else {
                        0
                    },
                    flatten: (expand && flatten)
                        .then(|| flatten_separator.as_deref().unwrap_or(" ")),
                    index,
                    icons,
                    collapse,
                }
                .grid(abbreviated.as_ref().unwrap_or(&value), 0);
                if let Some(kind) = empty {
                    grid = Grid::placeholder(format!("empty {kind}"), &theme);
                }
                grid.footer = grid.header.is_some()
                    && needs_footer(
                        config.footer_mode,
                        grid.count_rows(config.table.footer_inheritance),
                        screen.rows,
                    );
                let trim = match &config.table.trim {
                    TrimStrategy::Wrap { try_to_keep_words } => Trim::Wrap {
                        keep_words: *try_to_keep_words,
                    },
                    TrimStrategy::Truncate { suffix } => Trim::Truncate {
                        suffix: suffix.clone().unwrap_or_default(),
                    },
                };
                timer.lap("model");
                let faces = if mono { fonts.mono } else { fonts.sans };
                let settings = Settings {
                    trim,
                    // Collapsed tables draw lines between all their rows.
                    border: border.map(|border| Border {
                        rows: border.rows || collapse,
                        ..border
                    }),
                    line_color: theme.foreground(&styles.named("separator"), theme.muted),
                    padding: (config.table.padding.left, config.table.padding.right),
                    header_on_separator: config.table.header_on_separator,
                    collapse,
                    // An image that leaves the terminal has nothing behind it.
                    backdrop: as_png || copy,
                };
                let rows = grid.rows.len();
                within_budget(budget, rows, |keep| {
                    grid.cut(keep);
                    draw::lay_out(&grid, &fonts, faces, &theme, &screen, settings.clone())
                })
            }
        }
        .map_err(LabeledError::new)?;
        if drawing.rows == 0 {
            return fallback(engine, call, value, metadata);
        }
        timer.lap("layout");
        if copy {
            let (width, height, rgba) =
                draw::whole_rgba(&drawing, &theme, &fonts).map_err(LabeledError::new)?;
            plugin.copy_image(width, height, rgba).map_err(|err| {
                LabeledError::new("couldn't copy the image to the clipboard").with_label(err, head)
            })?;
            debug(&format!(
                "copied {width} x {height} pixels; {}",
                timer.report()
            ));
            if !as_png {
                return Ok(PipelineData::empty());
            }
        }

        if as_png {
            let mut rasterizer = Rasterizer::new(&fonts);
            let png =
                draw::whole_png(&drawing, &theme, &mut rasterizer).map_err(LabeledError::new)?;
            debug(&format!(
                "{} x {} cells, {} bytes of PNG; {}",
                drawing.columns,
                drawing.rows,
                png.len(),
                timer.report()
            ));
            return Ok(Value::binary(png, head).into_pipeline_data());
        }

        // A sixel terminal gets the whole drawing as one image: terminals
        // disagree on where the cursor goes after one, so bands could leave
        // gaps.
        if let Some(caps) = caps.as_ref().filter(|caps| !caps.kitty) {
            let (width, height, rgba) =
                draw::whole_rgba(&drawing, &theme, &fonts).map_err(LabeledError::new)?;
            timer.lap("paint");
            let out =
                sixel::encode(rgba, width, height, caps.background).map_err(LabeledError::new)?;
            timer.lap("sixel");
            debug(&format!(
                "{width} x {height} pixels of sixel, {} bytes; {}",
                out.len(),
                timer.report()
            ));
            return Ok(Value::string(out, head).into_pipeline_data());
        }

        // A terminal on this machine reads raw pixels from temporary files,
        // which skips PNG on both sides and base64 in between.
        let temp_files = caps.as_ref().is_some_and(|caps| caps.temp_files);
        if temp_files {
            kitty::sweep_temp_files();
        }
        // A band must fit on the screen: `place` makes room with newlines and
        // moves back up, and moving up stops at the top.
        let band_rows = BAND_ROWS.min(screen.rows.saturating_sub(1)).max(1);
        let bands = draw::paint_bands(&drawing, &theme, &fonts, band_rows, |canvas| {
            if temp_files {
                kitty::temp_file(&canvas.rgba, canvas.width, canvas.height)
            } else {
                draw::png(&canvas).map(kitty::Payload::Png)
            }
        })
        .map_err(LabeledError::new)?;
        timer.lap(if temp_files { "paint" } else { "paint+png" });
        let mut out = String::new();
        let last = bands.len().saturating_sub(1);
        for (index, (rows, payload)) in bands.iter().enumerate() {
            kitty::place(payload, drawing.columns, *rows, index == last, &mut out);
        }
        timer.lap("escape");
        debug(&format!(
            "{} x {} cells, {} bytes; {}",
            drawing.columns,
            drawing.rows,
            out.len(),
            timer.report()
        ));
        Ok(Value::string(out, head).into_pipeline_data())
    }
}

/// `display_output` hooks running in this plugin process. A hook that calls
/// `render` would otherwise run itself again.
static HOOKS_RUNNING: AtomicUsize = AtomicUsize::new(0);

/// Counts a running hook until dropped, even when the hook fails.
struct HookGuard;

impl HookGuard {
    fn enter() -> Self {
        HOOKS_RUNNING.fetch_add(1, Ordering::SeqCst);
        HookGuard
    }
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        HOOKS_RUNNING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Run the `display_output` hook on a string that carries a content type,
/// such as the output of `to json`, the way nushell does before showing
/// it, so a hook that colors JSON or nuon colors the drawing too. Only for
/// drawing: the fallback hands `table` the value as it came. Other values
/// skip it, since a hook ends in `table`, which would turn data into text.
/// `None` when the hook doesn't apply or fails.
fn display_hook(
    engine: &EngineInterface,
    value: &Value,
    metadata: Option<&PipelineMetadata>,
) -> Option<String> {
    if !matches!(value, Value::String { .. })
        || metadata.is_none_or(|metadata| metadata.content_type.is_none())
        || HOOKS_RUNNING.load(Ordering::SeqCst) > 0
    {
        return None;
    }
    let config = match engine.get_config() {
        Ok(config) => config,
        Err(err) => {
            debug(&format!("skipping `display_output`: {err}"));
            return None;
        }
    };
    let hook = match &config.hooks.display_output {
        Some(hook @ Value::Closure { val, .. }) => Spanned {
            item: (**val).clone(),
            span: hook.span(),
        },
        Some(_) => {
            debug("skipping `display_output`: a plugin can only run a closure hook");
            return None;
        }
        None => return None,
    };
    let span = value.span();
    let _guard = HookGuard::enter();
    let output = match engine.eval_closure_with_stream(
        &hook,
        Vec::new(),
        PipelineData::value(value.clone(), metadata.cloned()),
        true,
        false,
    ) {
        Ok(output) => output.into_value(span),
        Err(err) => Err(err),
    };
    match output {
        Ok(Value::String { val, .. }) => Some(val),
        Ok(other) => {
            debug(&format!(
                "ignoring `display_output`: it returned {}, not a string",
                other.get_type()
            ));
            None
        }
        Err(err) => {
            debug(&format!("`display_output` failed: {err}"));
            None
        }
    }
}

/// Lay out the first `keep` of `count` rows or lines, with `keep` falling
/// until the drawing is within `budget` pixels, or nothing but the note is
/// left.
fn within_budget(
    budget: Option<u64>,
    count: usize,
    mut lay_out: impl FnMut(usize) -> Result<Drawing, String>,
) -> Result<Drawing, String> {
    let mut keep = count;
    let mut drawing = lay_out(keep)?;
    let Some(budget) = budget else {
        return Ok(drawing);
    };
    while keep > 0 {
        let pixels = u64::from(drawing.columns)
            * u64::from(drawing.cell.0)
            * u64::from(drawing.rows)
            * u64::from(drawing.cell.1);
        if pixels <= budget {
            break;
        }
        // Rows cost about the same, so keep the share that fits, and at
        // least one fewer each time.
        let share = budget as f64 / pixels as f64;
        keep = ((keep as f64 * share) as usize).min(keep - 1);
        drawing = lay_out(keep)?;
    }
    Ok(drawing)
}

/// `--index` as `table` reads it: true, or no flag, counts rows from 0; a
/// number counts from there; false hides the index column.
fn index_flag(call: &EvaluatedCall) -> Result<Option<usize>, LabeledError> {
    match call.get_flag_value("index") {
        None | Some(Value::Nothing { .. }) => Ok(Some(0)),
        Some(Value::Bool { val, .. }) => Ok(val.then_some(0)),
        Some(value @ Value::Int { val, .. }) => usize::try_from(val).map(Some).map_err(|_| {
            LabeledError::new("`--index` can't be negative")
                .with_label("a negative number", value.span())
        }),
        Some(value) => Err(LabeledError::new("`--index` takes a bool or a number")
            .with_label(format!("found {}", value.get_type()), value.span())),
    }
}

/// `--abbreviated` as `table` does it: the first and last `keep` rows of a
/// list, or fields of a record, with a `...` row between. `None` when
/// nothing changes.
fn abbreviate(value: &Value, keep: usize) -> Option<Value> {
    let span = value.span();
    let dots = || Value::string("...", span);
    match value {
        Value::List { vals, .. } if keep == 0 => Some(Value::list(Vec::new(), span)),
        Value::List { vals, .. } if vals.len() >= keep.saturating_mul(2) => {
            let head = &vals[..keep];
            let tail = &vals[vals.len() - keep..];
            let all_records = head
                .iter()
                .chain(tail)
                .all(|val| matches!(val, Value::Record { .. }));
            // A list of records gets `...` in every column.
            let dummy = match head.first() {
                Some(Value::Record { val, .. }) if all_records => Value::record(
                    val.columns().map(|key| (key.clone(), dots())).collect(),
                    span,
                ),
                _ => dots(),
            };
            let mut vals = head.to_vec();
            vals.push(dummy);
            vals.extend_from_slice(tail);
            Some(Value::list(vals, span))
        }
        Value::Record { val, .. } if val.len() > keep.saturating_mul(2).saturating_add(1) => {
            let mut record = Record::with_capacity(keep * 2 + 1);
            for (key, val) in val.iter().take(keep) {
                record.push(key.clone(), val.clone());
            }
            record.push("...", dots());
            for (key, val) in val.iter().skip(val.len() - keep) {
                record.push(key.clone(), val.clone());
            }
            Some(Value::record(record, span))
        }
        _ => None,
    }
}

/// Whether the header shows again below the rows, per `footer_mode`, as
/// `table` decides it. `auto` compares the rows with the screen's height.
fn needs_footer(mode: FooterMode, rows: usize, screen_rows: u32) -> bool {
    match mode {
        FooterMode::Never => false,
        FooterMode::Always => true,
        FooterMode::RowCount(limit) => rows as u64 > limit,
        FooterMode::Auto => u64::from(screen_rows) <= rows as u64,
    }
}

/// The flags `render` shares with `table`, passed on when it falls back.
const TABLE_FLAGS: &[&str] = &[
    "width",
    "index",
    "expand",
    "expand-deep",
    "flatten",
    "flatten-separator",
    "abbreviated",
    "icons",
    "theme",
    "collapse",
];

/// Hand the value to `table`, with the flags they share, for terminals that
/// can't show images.
fn fallback(
    engine: &EngineInterface,
    call: &EvaluatedCall,
    value: Value,
    metadata: Option<PipelineMetadata>,
) -> Result<PipelineData, LabeledError> {
    let table = engine
        .find_decl("table")?
        .ok_or_else(|| LabeledError::new("`table` isn't available to fall back on"))?;
    let mut table_call = EvaluatedCall::new(call.head);
    table_call.named = call
        .named
        .iter()
        .filter(|(name, value)| {
            // `--theme card` is `render`'s own; `table` uses its configured
            // theme.
            let card = name.item == "theme"
                && matches!(value, Some(Value::String { val, .. }) if val.eq_ignore_ascii_case(crate::border::CARD));
            TABLE_FLAGS.contains(&name.item.as_str()) && !card
        })
        .cloned()
        .collect();
    Ok(engine.call_decl(
        table,
        table_call,
        PipelineData::value(value, metadata),
        true,
        false,
    )?)
}

/// A note on stderr when `NU_PLUGIN_RENDER_DEBUG` is set: why drawing was
/// skipped, or where the time went.
fn debug(note: &str) {
    if std::env::var_os("NU_PLUGIN_RENDER_DEBUG").is_some() {
        eprintln!("render: {note}");
    }
}

/// Time spent per phase, summed over bands.
struct Timer {
    last: std::time::Instant,
    phases: Vec<(&'static str, std::time::Duration)>,
}

impl Timer {
    fn new() -> Self {
        Timer {
            last: std::time::Instant::now(),
            phases: Vec::new(),
        }
    }

    fn lap(&mut self, phase: &'static str) {
        let now = std::time::Instant::now();
        let spent = now - self.last;
        self.last = now;
        match self.phases.iter_mut().find(|(name, _)| *name == phase) {
            Some((_, total)) => *total += spent,
            None => self.phases.push((phase, spent)),
        }
    }

    fn report(&self) -> String {
        self.phases
            .iter()
            .map(|(name, spent)| format!("{name} {:.1}ms", spent.as_secs_f64() * 1000.0))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Styles from `color_config`, its closures, and `LS_COLORS`.
struct EngineStyles<'a> {
    engine: &'a EngineInterface,
    config: &'a Config,
    colors: HashMap<String, Style>,
    closures: HashMap<String, Spanned<Closure>>,
    /// Closure results, by type and the value's text.
    computed: HashMap<(String, String), Style>,
    /// `$env.config.ls.use_ls_colors`.
    use_ls_colors: bool,
    ls_colors: LsColors,
    cwd: PathBuf,
    path_columns: Vec<String>,
}

impl<'a> EngineStyles<'a> {
    fn new(
        engine: &'a EngineInterface,
        config: &'a Arc<Config>,
        metadata: Option<&PipelineMetadata>,
    ) -> Self {
        let mut colors = HashMap::new();
        let mut closures = HashMap::new();
        for (key, value) in &config.color_config {
            match value {
                Value::Closure { val, .. } => {
                    closures.insert(
                        key.clone(),
                        Spanned {
                            item: (**val).clone(),
                            span: value.span(),
                        },
                    );
                }
                other => {
                    if let Some(style) = style_of(other) {
                        colors.insert(key.clone(), style);
                    }
                }
            }
        }
        // Without `$env.LS_COLORS`, nushell's own colors, as `table` uses.
        let ls_colors = nu_utils::get_ls_colors(
            engine
                .get_env_var("LS_COLORS")
                .ok()
                .flatten()
                .and_then(|value| value.coerce_into_string().ok()),
        );
        EngineStyles {
            engine,
            config,
            colors,
            closures,
            computed: HashMap::new(),
            use_ls_colors: config.ls.use_ls_colors,
            ls_colors,
            cwd: engine
                .get_current_dir()
                .map(PathBuf::from)
                .unwrap_or_default(),
            path_columns: metadata.map(|m| m.path_columns.clone()).unwrap_or_default(),
        }
    }
}

/// The style a `color_config` value names: a color name, hex, or record.
fn style_of(value: &Value) -> Option<Style> {
    match value {
        Value::String { val, .. } => Some(lookup_ansi_color_style(val)),
        Value::Record { .. } => Some(color_record_to_nustyle(value)),
        _ => None,
    }
}

impl Styles for EngineStyles<'_> {
    fn value(&mut self, key: &str, value: &Value) -> Style {
        let Some(closure) = self.closures.get(key) else {
            return self.colors.get(key).copied().unwrap_or_default();
        };
        let cache_key = (key.to_string(), value.to_abbreviated_string(self.config));
        if let Some(style) = self.computed.get(&cache_key) {
            return *style;
        }
        let style =
            match self
                .engine
                .eval_closure(closure, vec![value.clone()], Some(value.clone()))
            {
                Ok(result) => style_of(&result).unwrap_or_default(),
                Err(err) => {
                    debug(&format!("the `{key}` color closure failed: {err}"));
                    Style::default()
                }
            };
        self.computed.insert(cache_key, style);
        style
    }

    fn named(&self, key: &str) -> Style {
        self.colors.get(key).copied().unwrap_or_default()
    }

    fn path(&mut self, column: &str, text: &str) -> Option<Style> {
        if !self.use_ls_colors
            || !self
                .path_columns
                .iter()
                .any(|path_column| path_column == column)
        {
            return None;
        }
        // As `table` does: the file's own metadata, without following a
        // link, and a path with no style keeps the default color rather
        // than taking the `string` color.
        let metadata = std::fs::symlink_metadata(self.cwd.join(text)).ok();
        Some(
            self.ls_colors
                .style_for_path_with_metadata(text, metadata.as_ref())
                .map(lscolors::Style::to_nu_ansi_term_style)
                .unwrap_or_default(),
        )
    }

    fn icon(&self, path: &str) -> (char, Style) {
        // The full path, since devicons looks at the file to tell a
        // directory, and this process doesn't share nushell's directory.
        let icon = devicons::icon_for_file(&self.cwd.join(path), &None);
        (icon.icon, lookup_ansi_color_style(icon.color))
    }
}

#[cfg(test)]
mod tests {
    use nu_protocol::record;

    use super::*;

    fn ints(range: std::ops::Range<i64>) -> Value {
        Value::test_list(range.map(Value::test_int).collect())
    }

    #[test]
    fn abbreviates_lists_like_table() {
        let dots = Value::test_string("...");
        let expected = Value::test_list(vec![
            Value::test_int(0),
            Value::test_int(1),
            dots.clone(),
            Value::test_int(8),
            Value::test_int(9),
        ]);
        assert_eq!(abbreviate(&ints(0..10), 2), Some(expected));
        // `table` adds the `...` row once the list fills both ends.
        assert_eq!(
            abbreviate(&ints(0..4), 2).map(|list| list.as_list().map(<[Value]>::len).ok()),
            Some(Some(5))
        );
        assert_eq!(abbreviate(&ints(0..3), 2), None);
        assert_eq!(
            abbreviate(&ints(0..3), 0),
            Some(Value::test_list(Vec::new()))
        );
    }

    #[test]
    fn footer_follows_footer_mode() {
        assert!(!needs_footer(FooterMode::RowCount(5), 5, 40));
        assert!(needs_footer(FooterMode::RowCount(5), 6, 40));
        assert!(needs_footer(FooterMode::Auto, 40, 40));
        assert!(!needs_footer(FooterMode::Auto, 39, 40));
        assert!(!needs_footer(FooterMode::Never, 1000, 40));
    }

    #[test]
    fn abbreviates_records_like_table() {
        let record = Value::test_record(record! {
            "a" => Value::test_int(1),
            "b" => Value::test_int(2),
            "c" => Value::test_int(3),
            "d" => Value::test_int(4),
        });
        let expected = Value::test_record(record! {
            "a" => Value::test_int(1),
            "..." => Value::test_string("..."),
            "d" => Value::test_int(4),
        });
        assert_eq!(abbreviate(&record, 1), Some(expected));
        assert_eq!(abbreviate(&record, 2), None);
    }
}
