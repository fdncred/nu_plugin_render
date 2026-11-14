# TODO

What `render` doesn't do yet, and what nobody has checked on a real
terminal.

- **Sixel.** Nobody has tried it on a real sixel terminal. The tests decode
  its output back into the image. In a pty that answers like a sixel
  terminal, `render` draws with sixel, and a decoder written separately from
  `icy_sixel` shows the image as drawn. There, `$env | render -e` is
  1190 × 11816 pixels and encodes in about 200 ms, to 4 MB. Some terminals
  cap a sixel image's size, xterm at 1000 × 1000 pixels by default, which
  cuts a tall drawing short. `--budget` keeps it under the cap. A sixel
  terminal that doesn't report its cell size falls back to `table`.
- **Windows.** The probe goes through the console API. It reads `CONIN$` in
  terminal-input mode, with a timeout. So Windows Terminal, which has sixel,
  and WezTerm, which has kitty graphics, should draw. It's only
  compile-checked. Nobody has run it on Windows.
- **tmux.** Inside tmux 3.4 or later, `render` draws with sixel when the
  terminal tmux runs in has sixel, as tmux's `client_termfeatures` reports,
  and falls back to `table` otherwise. Kitty graphics don't pass through
  tmux. Nobody has tried tmux on a sixel terminal yet.
- **zellij.** Untested.
- **Speed.** `$env | render -e` at 143 columns is 142 × 461 cells, about 34
  million pixels. It takes about 130 ms in the plugin, and Rex then takes
  about 230 ms to show it. `table -e` takes about 20 ms. About 57 ms of the
  plugin's time is drawing:
  - probe 12 ms
  - model 10 ms
  - layout 4 ms
  - paint and write files 31 ms

  The rest is Nushell sending the value and the config to the plugin.
  Inline PNG, for a terminal on another machine, costs more on both sides:
  191 ms and 347 ms. Rex refuses shared memory, `t=s`.
- **Clipboard on Linux.** Copied data lasts only while the process that
  copied it holds the clipboard. The plugin keeps running and keeps its
  clipboard open. When it stops, with nushell or `plugin stop render`, the
  image goes too, unless a clipboard manager has taken it.
- **Leftover files.** Output captured and never displayed leaves its
  temporary files until the next `render` call. With no next call, they stay
  until the system cleans its temporary folder.
- **Type-ahead.** The first `render` in a session reads `/dev/tty` in raw
  mode and throws away whatever isn't an answer, so it loses keys you type
  during it. The plugin asks nushell to keep it running instead of stopping
  it when idle, so later calls reuse its answers and lose nothing. It holds
  about 60 MiB after drawing `$env | render -e`. After you change the
  terminal's font size, `plugin stop render` makes it probe again.
- **Light theme.** Checked in images, but not yet on a terminal with a light
  background. Rex doesn't answer OSC 11, so it always gets the dark theme.
- **Plugin config.** Not wired up yet, so there's no way to pick a font
  family.
