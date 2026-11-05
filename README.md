# de

`de` is a tiny inline directory explorer and navigator. It shows the current
directory, lets you walk into or out of folders, and changes the shell's working
directory only when you confirm.

![Navigating directories with de](demo/de.gif)

At normal terminal widths, the left pane lists the current directory. The right
pane previews the highlighted directory or file. Below 58 columns, `de` shows
one pane. Press `v` on a file to use the full-width inline viewer at any width.
Directory listings show modification times when enough space is available.

It is intentionally not a full file manager. Press `o` to open the highlighted
file in its default application. There are no delete, rename, copy, or built-in
edit commands.

## Install

Install from crates.io with Cargo (the package is named `de-cli`, but the
installed command is `de`):

```sh
cargo install de-cli --locked
```

> `--locked` tells Cargo to use the exact dependency versions in the published
`Cargo.lock`—the same versions tested for this release—instead of resolving
newer compatible versions. It is optional, but makes the install reproducible.

Prebuilt archives for Linux and macOS on x86-64 and ARM64 are available from
[GitHub Releases](https://github.com/reyamira/de/releases). Each archive includes
`de`, this README, and the MIT license; SHA-256 checksums are published alongside
the archives. Extract the archive, then place `de` somewhere on your `PATH`.

### Or Build from source

```sh
cargo build --release
export PATH="$PWD/target/release:$PATH"
de
```

### Shell integration

The `de` executable cannot change its parent shell's working directory by
itself. `de init <shell>` prints a small wrapper function to stdout; it does not
modify your shell configuration. The commands below evaluate that function in
your current shell. Add the same command to the listed startup file to enable
`de` in future shells.

#### Bash

Run now, then add the same line to `~/.bashrc`:

```bash
eval "$(command de init bash)"
```

#### Zsh

Run now, then add the same line to `~/.zshrc`:

```zsh
eval "$(command de init zsh)"
```

#### Fish

Run now, then add the same line to `~/.config/fish/config.fish`:

```fish
command de init fish | source
```

`eval` and `source` load the generated wrapper into the current shell. The
`command de` calls explicitly invoke the executable even after that wrapper is
defined. The wrapper captures the directory printed by the executable and then
asks the parent shell to `cd` there.

## Controls

| Key | Action |
| --- | --- |
| `Up` / `Down`, `j` / `k` | Select an entry |
| `PageUp` / `PageDown` | Jump one visible page through the entries |
| `Right`, `l`, `Tab` | Make the previewed directory current |
| `Left`, `h`, `Backspace` | Go to the parent directory |
| `/` | Filter entries in the current directory |
| `s` | Toggle sorting by name or modification time |
| `Shift+S` | Toggle ascending or descending order |
| `v` | View the highlighted file inline |
| `m` (Markdown viewer) | Toggle source and rendered Markdown |
| `o` | Open the highlighted file with its default application |
| `Enter` | Change to the directory currently displayed |
| `.` | Toggle hidden entries |
| `w` | Save the current hidden and sort settings as defaults |
| `r` | Refresh |
| `Escape`, `q`, `Ctrl-C` | Cancel without changing directory |

The important distinction is that `Enter` accepts the directory in the header,
not the highlighted child. Use `Right` to explore and `Enter` when you have
arrived.

Press `o` on a file to open it with the operating system's default application.
On Linux, `de` uses `xdg-open`; on macOS, it uses `open`. Opening a file exits
the picker without changing the shell's working directory. Directories continue
to use the navigation controls above.

### File previews

The right pane and inline viewer support these file types:

- **Markdown:** The right pane shows a styled text excerpt. The full-width
  viewer uses native FMD rendering, including tables and supported local images.
- **JSON:** Formatted, colorized text that preserves key order and number text.
  Invalid JSON remains readable with an error notice.
- **Plain text:** UTF-8 text with terminal control characters replaced.
- **Images:** PNG, JPEG, GIF, WebP, and BMP in Kitty or Ghostty.
  Animated images show their first frame.

The full-width viewer has a thin border around its contents. Text and images
use the space inside that border, including after a terminal resize. A scrollbar
uses the rightmost content column when there is enough space.

In the viewer, use arrows or `j`/`k` to scroll. Use `PageUp`/`PageDown` for pages
and `Home`/`End` for the beginning or end. For Markdown, press `m` to switch
between the rendered page and source. The header shows the current mode.
Press `Escape` or `q` to return to the same selection. Press `o` to open the
file externally. During filter input, `v` and `m` remain query text.

Use the mouse wheel over the viewer to scroll three rows per notch. Click the
scrollbar track to jump, or drag the thumb to scroll. Mouse capture is active
only while the viewer is open. The viewer stays next to the prompt, subject to
normal terminal scrolling when it needs more rows. Mouse controls require a
terminal that answers cursor-position queries. If it does not answer promptly,
mouse capture stays off for that session; keyboard controls remain available.
After the terminal width shrinks, reflow can leave a fragment of the old frame
above the viewer.

Image previews require the Kitty graphics protocol. They are unavailable in
other terminals and inside tmux, GNU Screen, or Zellij. When `de` cannot show a
file, it explains the reason and shows the external-open option. Video playback
remains external.

The native FMD Markdown view requires Kitty or Ghostty without a terminal
multiplexer. It does not require a browser. The page is a raster image, so text
selection and clickable links are not available. Source mode does not require
terminal graphics support.

Markdown images must be local PNG, JPEG, GIF, WebP, or BMP files inside the
document folder or its subfolders. Remote assets are not loaded. Raw HTML is
inert. Animated GIFs show their first frame. Images are resized to at most 2,048
pixels per side. Each file is limited to 16 MiB and 16,777,216 pixels. Each
document is limited to 128 image occurrences, 32 MiB of image source data, and
33,554,432 decoded or rendered pixels in total.

FMD supports some Mermaid diagrams. Unsupported diagrams remain as source.
Rendering runs in the background. The viewer keeps the last viewport while it
prepares a new one. New requests replace queued work.

Text previews accept regular UTF-8 files up to 1 MiB. Styled text and source
previews show at most 20,000 lines. Image previews accept files up to 16 MiB
and 16,777,216 pixels, with each dimension at most 16,384 pixels.
Images use EXIF orientation and scale to at
most 2,048 pixels per side before terminal transfer. Press `r` in the picker to
reload a changed file.

Image preparation runs in a background worker. A loading message appears while
the image is prepared, and navigation remains available. New selections replace
queued work, and results from old selections do not replace the current preview.
The viewer sends compressed image data in bounded batches between input checks.

Filtering uses a case-insensitive substring match. Type after pressing `/`, use
the normal arrow and page keys to move through matches, and press `Backspace` to
edit the query. `Escape` clears an active filter first; press it again to cancel
`de`.

Name sorting is case-insensitive and alphabetical. Sort criterion and direction
are independent, so modification time can show either oldest or newest first.
Directories remain grouped above files in every mode, and the selected sort
carries into the preview pane and navigated directories.

## Configuration

Settings live in one `config.toml`. On both Linux and macOS it is
`$XDG_CONFIG_HOME/de/config.toml`, or `~/.config/de/config.toml` when
`XDG_CONFIG_HOME` is unset or not an absolute path. macOS does not use
`~/Library/Application Support`.

```sh
de config        # change settings with a live preview
de config path   # print the file's location
de config edit   # open it in $VISUAL or $EDITOR, creating it if needed
```

`de config` lists hidden entries, sort, order, the modified column, date format,
time format, and timezone beside the current directory, rendered with those
settings as you change them. Use `Up`/`Down` to pick a setting and
`Left`/`Right` to change it. `Enter` saves every setting shown, leaving other
settings and comments untouched; `Escape` cancels without writing anything.
A `custom_format` can only be written in the file itself, but once it exists
the picker offers `custom` as a date format and never removes it. Themes are
chosen with `de theme`.

`de config edit` falls back to `vi` when neither variable is set, and checks the
file after the editor exits so a typo is reported immediately rather than on the
next launch. The sections below describe what the file can contain.

## Defaults

Hidden entries, sort criterion, and sort direction reset to their defaults each
time `de` starts. To change what it starts with, set them up the way you like
and press `w`. That writes them to the `[defaults]` table in `config.toml`,
leaving other settings and comments untouched. Toggling during a session never changes the saved defaults
on its own.

You can also edit the table directly:

```toml
[defaults]
hidden = false
sort = "name"
order = "ascending"
```

`hidden` is `true` or `false`, `sort` accepts `name` or `modified`, and `order`
accepts `ascending` or `descending`. Omitted keys keep the values shown above.

## Themes

Run the interactive selector to preview the real picker with every built-in and
custom theme, then save the one you choose:

```sh
de theme
```

Use `Left`/`Right` or `Up`/`Down` to preview `auto`, `light`, `dark`, `mono`,
`ocean`, `forest`, `amber`, and `rose`. `Enter` saves the displayed theme;
`Escape` cancels without changing the saved choice.

For a one-time override, use `de --theme dark`. `DE_THEME=mono de` provides an
environment-level override. Precedence is command-line flag, environment,
saved choice, then `auto`.

The `auto` theme uses the terminal's default foreground and background, its
ANSI palette, and reverse video for selection. That adapts without trying to
guess the background color or waiting for a terminal query.

Create an editable custom theme with:

```sh
de theme create midnight
```

That adds a complete starting palette to `config.toml` without replacing other
settings or comments:

```toml
[theme]
selected = "midnight"

[themes.midnight]
extends = "dark"
accent = "#7dd3fc"
text = "default"
muted = "#94a3b8"
title = "#c4b5fd"
error = "#fb7185"
symlink = "#f0abfc"
selection_fg = "#0f172a"
selection_bg = "#67e8f9"
reverse_selection = false
dim_muted = false
```

Every field after `extends` is optional. A custom theme can extend any built-in
theme and override only what it needs. Colors accept `#RRGGBB`, `default`, or
ANSI names such as `blue`, `light-cyan`, and `dark-gray`. After editing, run
`de theme` to preview it alongside the built-ins. Custom names also work with
`--theme` and `DE_THEME`.

## Date and time

Modification timestamps use local time in `YYYY-MM-DD HH:MM` form by default.
Change their presentation in the same `config.toml` file:

```toml
[display]
modified = true
date_format = "iso"
time_format = "24h"
timezone = "local"
```

`date_format` accepts `iso`, `us`, `european`, `relative`, or `custom`:

| Setting | Example |
| --- | --- |
| `iso` | `2026-08-25 15:02` |
| `us` with `time_format = "12h"` | `08/25/26 03:02 PM` |
| `european` | `25/08/26 15:02` |
| `relative` | `4 min ago`, `yesterday`, `3 months ago` |

`time_format` accepts `12h` or `24h`, and `timezone` accepts `local` or `utc`.
Set `modified = false` to hide the column entirely. Sorting by modification time
still uses the exact filesystem timestamp regardless of its presentation.

For complete control, use a
[Chrono strftime format](https://docs.rs/chrono/latest/chrono/format/strftime/):

```toml
[display]
date_format = "custom"
custom_format = "%b %e, %Y at %l:%M %p"
timezone = "local"
```

Custom formats replace both the date and time presets; relative timestamps do
not use the clock or timezone settings. Column width follows the rendered values
automatically. When the terminal is too narrow to retain a readable filename
column, timestamps collapse away with the rest of the responsive layout.

## Built with

- [Ratatui](https://ratatui.rs/) renders the interactive panes and layout.
- [Crossterm](https://github.com/crossterm-rs/crossterm) handles terminal input,
  raw mode, cursor movement, and colors.
- [Clap](https://docs.rs/clap/) parses arguments and generates the styled help,
  version, subcommand, and error output.
- [toml_edit](https://docs.rs/toml_edit/) reads and updates configuration while
  retaining the user's formatting and comments.

The small relative-coordinate backend in `src/backend.rs` keeps the picker
inline without requiring the terminal to answer an absolute cursor-position
query. This is useful in PTYs and layered terminal environments.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

The README demo is driven by the checked-in Terminal Control tape. To replay it
after building the release binary:

```sh
cargo build --release
termctrl play demo/de.tape
```

The tape creates and removes its own synthetic fixture under `/tmp`; its private
`.termctrl` timeline and intermediate MP4 are gitignored. `demo/de.gif` is the
small public artifact embedded above.

The UI is rendered on stderr so stdout remains a clean selected-path protocol
for the shell wrapper. On Unix, path bytes are preserved by the executable; like
most command-substitution integrations, directories whose names end in newline
characters are outside the supported shell-wrapper boundary.
