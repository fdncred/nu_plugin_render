# Developing nu_plugin_render

How `render` works inside, what it depends on, how it is tested, and how a
release goes out. [README.md](README.md) covers using it.

## How it works

Six modules turn a value into pixels, and none of them touch the terminal:

- `model` turns a value into a grid of styled text.
- `layout` measures it with real fonts and fits the columns.
- `paint` draws RGBA bands.
- `draw` makes cell-sized bands, encodes PNG, and streams a PNG of the
  whole drawing.
- `ansi` turns escape codes into styled spans.
- `text` lays a string out on terminal cells.

Three modules talk to the terminal:

- `term` probes it. It takes the foreground from Nushell, then sends queries
  to `/dev/tty` in raw mode: the kitty graphics query, `CSI 16t`, `CSI 14t`,
  OSC 11, a three-byte image in a temporary file, and DA1. Any failure falls
  back to `table`. The plugin keeps the answers, so later calls only read
  the window's size and never read from the terminal.
- `kitty` writes each band with `C=1`, after newlines make room, so the
  screen scrolls before the image goes up. If the terminal read the
  temporary file during the probe, it runs on this machine, and each band
  goes as raw RGBA in a temporary file with `f=32,t=t`. The terminal reads
  the file and deletes it. Otherwise each band goes inline as base64 PNG,
  with `f=100`.
- `sixel` is for a terminal that has no kitty graphics, lists sixel in its
  device attributes and reports its cell size. It sends the whole drawing as
  one image, pixel for pixel. When the terminal reports its background,
  `render` blends the image onto it first, so edges stay smooth. Otherwise
  it leaves out pixels less than half opaque. Colors come down to a
  256-color palette, without dithering.

`render` draws text at about the terminal's font size. A terminal that
reports cells under 24 pixels tall is counting points on a HiDPI screen, so
its images get twice the pixels.

The drawing goes out as images 40 cell rows tall. The cuts fall on cell
rows, so the bands join without seams. Every core paints bands, each thread
with its own glyph cache.

**Why 40 rows.** Every image costs the terminal time, so fewer, taller bands
show sooner. In Rex, `$env | render -e` at 143 columns took 141 ms to show
in 22-row bands, 240 ms in 4-row bands and 318 ms in 1-row bands.

Output that never reaches a terminal, such as `render | ignore`, leaves
temporary files nobody reads. The next call deletes them once they're a
minute old.

`render` bundles no fonts. It finds the system's through `fontdb`. For each
character the main face lacks, it looks through a list of fallback families:
Nerd Fonts first, then Apple Color Emoji, PingFang, Noto, Segoe UI Emoji and
others, then every installed face.

## Dependencies

Everything `render` draws with is MIT, Apache-2.0 or both:

- `swash` for shaping and glyphs
- `fontdb` for system fonts
- `png`
- `base64`
- `lscolors`
- `unicode-width`
- `libc`, on Unix
- `windows-sys`, on Windows, for the console the probe reads
- `nu-utils` for nushell's default `LS_COLORS`
- `arboard` with `image-data` and `wayland-data-control` for the clipboard,
  as nushell's `clip copy` uses it
- `devicons` for the `--icons` file icons, as `table --icons` uses it
- `icy_sixel`, the sixel encoder

Their dependencies add:

- `slotmap`, Zlib, through `fontdb`
- `moxcms` and `pxfm`, BSD-3-Clause OR Apache-2.0, through `arboard`'s
  `image`
- `quantette`, `palette` and the rest of `icy_sixel`'s tree, under MIT
  and/or Apache-2.0, or Zlib OR Apache-2.0 OR MIT

The other licenses `cargo tree` shows come from the Nushell plugin crates,
which every plugin depends on:

- `brotli`, BSD-3-Clause
- `option-ext`, MPL-2.0
- ICU, Unicode-3.0
- `self_cell`
- `is_ci`, ISC

## Tests

`cargo test` checks the terminal-reply parser, the ANSI parser, text
clusters and widths, and the kitty escape codes.

It also paints two drawings whole and in bands of 1, 2, 7 and 28 cell rows,
and checks that the joined bands match the whole drawing byte for byte:

- a table with nested tables and two-line cells
- `grid`-like terminal text with icons, highlights and rules, shrunk and
  wrapped

A third test decodes the streamed `--png` output and compares it with the
drawing painted whole. These tests need the system's fonts.

To test without a registered `render` in the way, give nushell an empty
registry:

```nu
nu --plugin-config /tmp/empty.msgpackz --plugins '[target/release/nu_plugin_render]'
```

## Releasing

[RELEASE.md](RELEASE.md) has every step, from setting the version to
`cargo publish`.
