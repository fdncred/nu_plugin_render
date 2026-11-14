# nu_plugin_render

A prototype Nushell plugin. `render` draws a table as pixels and shows it in
the terminal with the kitty graphics protocol, so columns line up exactly,
emoji and CJK keep their width, and nested lists and records show as nested
cards. A string draws as terminal text in its own ANSI colors, so the output
of `grid` keeps its layout. When the terminal can't show kitty graphics,
`render` prints what `table` prints.

```nu
ls | render
open data.json | render -e         # nested lists and records as nested cards
ls | grid -c -i name | render      # text that lines itself up stays lined up
ls | render --png | save ls.png   # the picture, without a terminal
ls | render --copy                # the picture, on the clipboard
```

## Try it

```nu
cargo build --release
nu --plugins '[target/release/nu_plugin_render]'
```

or `plugin add target/release/nu_plugin_render` and `plugin use render`.

Built against `nu-plugin` 0.117.0, which loads in Nushell 0.117.x.

## Flags

| Flag | What it does |
| --- | --- |
| `--png` | Return the whole drawing as PNG bytes instead of drawing it, cut to the card. It is painted and encoded a band at a time, so a tall table needs no more memory than one band. |
| `--copy` | Copy the drawing to the clipboard as an image, cut to the card, instead of drawing it. With `--png` too, it also returns the PNG. |
| `--mono` | Draw a table in a monospace font instead of a sans-serif one. Strings always draw in monospace. |
| `--width`, `-w` | The widest the drawing may be, in terminal columns. Defaults to the terminal's width, or 160 with `--png` or `--copy`. |
| `--budget` | Draw at most this many million pixels. A table stops at a whole row and a string at a whole line, ending in a note such as `… 1,941 more rows`. Without it, everything is drawn. |

These work as they do in `table`:

| Flag | What it does |
| --- | --- |
| `--expand`, `-e` | Nested lists and records draw as nested cards, however deep. Without it they show as summaries, such as `[table 3 rows]`. |
| `--expand-deep`, `-d` | How many levels `--expand` nests; deeper ones show their summary. |
| `--flatten` | With `--expand`, a list of plain values in a table's row shows as one line. A record's values don't flatten. |
| `--flatten-separator` | What `--flatten` joins with. Defaults to a space. |
| `--index`, `-i` | `false` hides the `#` column, `true` shows it, and a number starts it there. Nested tables count from 0. |
| `--abbreviated`, `-a` | Only this many rows at the top and bottom, with a `...` row between. Defaults to `$env.config.table.abbreviated_row_count`. |
| `--icons`, `-o` | A file icon before each path that `LS_COLORS` styles, from devicons, as `table --icons` draws it. |
| `--theme`, `-t` | Draw the lines of a `table` theme, such as `rounded`, `heavy` or `psql`, instead of the card. `card`, the default, has none. |
| `--list`, `-l` | List the themes `--theme` takes, `card` first. |

When `render` falls back to `table`, it passes these and `--width` along.

Set `NU_PLUGIN_RENDER_DEBUG=1` in the environment Nushell starts with to see,
on stderr, why `render` fell back to `table`, or how long each phase took.

## What it draws

- **Header and index:** a header band, and the `#` index column per
  `$env.config.table.index_mode`.
- **Rows:** zebra stripes on a rounded card.
- **Colors:** `color_config` colors, closures included.
- **`LS_COLORS`:** used for path columns such as `ls`'s `name`, as `table`
  does:
  - nushell's own defaults when `$env.LS_COLORS` isn't set
  - nothing when `$env.config.ls.use_ls_colors` is off
  - the default text color for a path with no style
  - a background, such as the one on `README.md`, drawn as a highlight
- **ANSI text in cells:** text with ANSI escapes in a cell keeps its colors,
  bold, and backgrounds.
- **Numbers:** right-aligned with tabular digits. Columns of floats line up
  on the decimal point, at `float_precision`.
- **Nested values:** with `--expand`, lists and records nest as smaller
  cards.
- **Footer:** the header again below the rows, per `footer_mode`, counting
  nested rows too with `footer_inheritance`.
- **Empty values:** an empty list or record draws a dimmed `empty list` or
  `empty record` with `show_empty`, and nothing without it.
- **Missing values:** a record in a list without one of the list's columns
  shows `$env.config.table.missing_value_symbol`, centered, in the `empty`
  color.
- **Fitting:** when the table is wider than the screen:
  - The widest text columns get narrower. Their text wraps, between words
    when `wrapping_try_keep_words` is on, or with `trim.methodology:
    truncating`, is cut short and ends in `truncating_suffix`. Number
    columns never shrink.
  - Each column's nested tables get the room the other columns leave.
  - A nested table shows its summary, such as `{record 39 fields}`, when it
    would get less room than about two dozen characters, or when fitting it
    would cut any column but its last (such as a record's keys).
  - If the table is still too wide, the widest nested tables collapse one
    at a time until it fits.
- **Multi-line strings:** they keep their lines.
- **Theme:** the card is dark, or light when the terminal reports a light
  background (OSC 11).
- **Lines:** with `--theme`, each theme's lines as `table` draws them in
  characters:
  - frame, lines between columns, under the header, and between rows
  - rounded, square or heavy corners
  - thin, heavy, double or dotted strokes, and hearts for `with_love`
  - the gaps between `restructured`'s columns

  There is no card behind the lines, so the text sits on the terminal's own
  background. The lines take `color_config.separator`, or a muted text color
  without one. `padding` sets the space beside each column, one character
  being the card's spacing. With `header_on_separator`, the header sits in a
  gap on the top line, except in `markdown` and `restructured`.

A string, such as `grid`'s output, draws as terminal text instead:
- **Font and layout:** a monospace font, every character in the cells
  `unicode-width` gives it, tabs every 8 cells.
- **Colors:** foreground, background, bold, dim, underline and reverse from
  its ANSI escapes. Other escape sequences, such as OSC 8 links, are dropped.
- **`display_output` hook:** a string that carries a content type, such as
  the output of `to json` or `open --raw`, goes through
  `$env.config.hooks.display_output` first, as it would before nushell
  shows it. A hook that colors JSON with `bat` or nuon with `nu-highlight`
  colors the drawing too.
  - Other values skip the hook, since a hook ends in `table`, which would
    turn them into text.
  - A `render` inside the hook doesn't run the hook again.
  - Only a drawing goes through the hook. When `render` falls back, `table`
    gets the string as it came, and nushell's own hook runs as usual.
  - A hook written as a string, not a closure, can't run from a plugin and
    is skipped.
- **Box drawing:** `│` and `─` draw as thin lines that join from line to
  line, in a quiet rule color unless the text colors them.
- **Icons:** Nerd Font icons come from an installed Nerd Font. Private-use
  characters ask Nerd Fonts first, since other fonts put unrelated symbols
  there.
- **Symbols:** a one-cell symbol the font lacks, such as `♥`, is drawn from
  a text font. A two-cell one, such as `⚡`, comes from a color emoji font.
- **Fitting:** text wider than the screen shrinks evenly, to no less than
  three quarters of the terminal's size. Lines still too long wrap, as in a
  terminal.

## How it works

From value to pixels, nothing touches the terminal:

- `model`: value to grid of styled text
- `layout`: measure with real fonts, fit columns
- `paint`: draw RGBA bands
- `draw`: cell-sized bands, PNG, and a streamed PNG of the whole drawing
- `ansi`: escape codes to styled spans
- `text`: a string laid out on terminal cells
- `term` probes the terminal: it takes the foreground from Nushell, then sends
  queries to `/dev/tty` in raw mode: the kitty graphics query, `CSI 16t`,
  `CSI 14t`, OSC 11, a three-byte image in a temporary file, and DA1. Any
  failure falls back to `table`.
- `kitty` writes each band with `C=1`, after newlines make room, so the
  screen scrolls before the image goes up. When the terminal read the
  temporary file in the probe, which means it runs on this machine, a band
  goes as raw RGBA in a temporary file (`f=32,t=t`), which the terminal reads
  and deletes. Otherwise it goes inline as base64 PNG (`f=100`).

Details:
- **Image size:** text is drawn at about the terminal's font size. Terminals
  that report cells under 24 pixels tall are counting points on a HiDPI
  screen, so those images get twice the pixels.
- **Bands:** the drawing goes out as images 40 cell rows tall. They are cut
  on cell rows, so they join without seams, and are painted on every core,
  each thread with its own glyph cache.
- **Why 40 rows:** every image costs the terminal time, so fewer, taller
  bands show sooner. In Rex, `$env | render -e` at 143 columns took 141 ms
  to show in 22-row bands, 240 ms in 4-row bands, and 318 ms in 1-row
  bands.
- **Temporary files** that no terminal read, from output that never reached
  one (such as `render | ignore`), are deleted on the next call once they're
  a minute old.
- **Fonts:** they come from the system through `fontdb`; none are bundled.
  Each character the main face lacks is looked up in a list of fallback
  families, Nerd Fonts first, then Apple Color Emoji, PingFang, Noto, Segoe
  UI Emoji and others, then in every installed face.

## Dependencies

The rendering stack is MIT and/or Apache-2.0:

- `swash`: shaping and glyphs
- `fontdb`: system fonts
- `png`
- `base64`
- `lscolors`
- `unicode-width`
- `libc` (Unix)
- `nu-utils`: nushell's default `LS_COLORS`
- `arboard` with `image-data` and `wayland-data-control`: the clipboard, as
  nushell's `clip copy` uses it

Their trees add:
- `slotmap`: Zlib, via `fontdb`
- `moxcms` and `pxfm`: BSD-3-Clause OR Apache-2.0, via `arboard`'s `image`

The other licenses `cargo tree` shows come from the Nushell plugin crates,
which every plugin depends on:

- `brotli`: BSD-3-Clause
- `option-ext`: MPL-2.0
- ICU: Unicode-3.0
- `self_cell`
- `is_ci`: ISC

## Tests

`cargo test` checks the terminal-reply parser, the ANSI parser, text
clusters and widths, and the kitty escape codes.

It also paints two drawings both whole and in bands of 1, 2, 7 and 28 cell
rows, and checks that the joined bands match the whole drawing byte for
byte:
- a table with nested tables and two-line cells
- `grid`-like terminal text with icons, highlights and rules, shrunk and
  wrapped

A third test decodes the streamed `--png` output and checks it against the
drawing painted whole. These tests need the system's fonts.

To test without a registered `render` in the way, give nushell an empty
registry:

```sh
nu --plugin-config /tmp/empty.msgpackz --plugins '[target/release/nu_plugin_render]'
```

## Not done yet

- **sixel:** a sixel-only terminal, such as Windows Terminal or foot, falls
  back to `table`.
- **Windows:** there is no terminal probe yet, so it always falls back.
- **tmux, zellij:** no passthrough, so they fall back.
- **Rex:** the top of a tall drawing stays blank. `$env | render -e` at 148
  columns loses about 60% of its height, drawn at full resolution or at a
  quarter of the pixels. Rex still holds the images but doesn't draw them:
  replaying `render`'s output, Rex dropped only the oldest 8% of the bands
  at full resolution, to its 128 MiB limit, and none at a quarter.
  `--budget` keeps a drawing short enough. Ghostty shows the same output
  whole.
- **Speed:** `$env | render -e` at 143 columns (142 × 461 cells, about 34
  million pixels) takes about 130 ms in the plugin, and Rex then takes
  about 230 ms to show it. `table -e` takes about 20 ms. Of the plugin's
  time, about 57 ms is drawing:
  - probe 12 ms
  - model 10 ms
  - layout 4 ms
  - paint and write files 31 ms

  The rest is Nushell sending the value and the config to the plugin.
  - **Inline PNG:** for a terminal on another machine, it costs more on both
    sides: 191 ms and 347 ms.
  - **Shared memory (`t=s`):** Rex refuses it.
- **Clipboard on Linux:** copied data lasts only while the process that
  copied it holds the clipboard. The plugin keeps its clipboard open
  between calls, but when nushell stops the plugin, the image goes too,
  unless a clipboard manager has taken it.
- **Leftover files:** output captured and never displayed leaves its
  temporary files until the next `render` call (or the system's own cleanup
  if there is none).
- **Too wide:** a table whose number columns alone are wider than the screen
  still overflows it.
- **Type-ahead:** the probe reads `/dev/tty` in raw mode and throws away
  whatever isn't an answer, so keys typed while `render` runs are lost.
- **Light theme:** untested on a real terminal. Rex doesn't answer OSC 11,
  so only the parser is tested.
- **Not wired up:** `table`'s `--collapse`, and plugin config such as a
  font family.
