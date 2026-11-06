# Configuration

Return to the [README](../README.md) for installation and navigation controls.

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

Run the interactive selector to preview each built-in and custom theme with
browser and Markdown samples, then save the one you choose:

```sh
de theme
```

Use `Left`/`Right` or `Up`/`Down` to preview `auto`, `light`, `dark`, `mono`,
`ocean`, `forest`, `amber`, and `rose`. `Enter` saves the displayed theme;
`Escape` cancels without changing the saved choice.
Press `Tab` to change samples. Use `PageUp`/`PageDown` to see examples below
the visible area.

For a one-time override, use `de --theme dark`. `DE_THEME=mono de` provides an
environment-level override. Precedence is command-line flag, environment,
saved choice, then `auto`.

The `auto` theme uses the terminal's default foreground and background, its
ANSI palette, and reverse video for selection. That adapts without trying to
guess the background color or waiting for a terminal query.

Edit a theme with a live preview:

```sh
de theme edit            # edit the theme resolved by the usual precedence
de theme edit midnight   # edit a named theme
```

Use `Up`/`Down` or `j`/`k` to select a field. `Home` and `End` also move
through the fields. Press `Enter` or `e` to edit the selected value. Press
`Enter` to apply it, or `Escape` to restore it. An empty value accepts
inheritance from the base theme. Inherited fields show their actual values.
Accepting an unchanged value keeps inheritance. Press `r` or `Delete` to
reset the selected field to its inherited value.

For a color, press `c` or `Enter` to edit its text value. A swatch shows the
effective color. The accepted values for each color type are described below.

Press `Tab` to cycle the right panel through Browser, Markdown source, and
rendered Markdown. The fields keep focus. Use `PageUp`/`PageDown` to see
examples below the visible area. Both theme tools use the same samples.
Each section identifies the sample affected by its settings. Base affects all
samples; Syntax and Markdown affect Source; rendered colors and typography affect
Rendered Markdown. Interface affects Browser.
Narrow terminals hide the editor preview. Press `s` or `Ctrl-S` to save,
then confirm the save.
When you edit a built-in theme, `de` asks for a custom name before confirmation.
It does not write until you confirm.

Saving updates an existing custom theme in place and preserves comments and
unrelated settings. Saving does not select the theme. Use `de theme` to select it.
Press `Escape` or `q` in the field list to cancel without saving.

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

[themes.midnight.syntax]
text = "white"
comment = "gray"
keyword = "light-blue"
string = "light-cyan"
number = "light-magenta"
constant = "light-magenta"
entity = "light-blue"
support = "light-magenta"

[themes.midnight.markdown]
text = "white"
heading = "light-blue"
link = "light-magenta"
link_url = "gray"
code = "light-cyan"
punctuation = "gray"
marker = "light-cyan"

[themes.midnight.rendered]
background = "#111827"
text = "#f9fafb"
heading = "#f9fafb"
link = "#60a5fa"
code_background = "#1f2937"
code_text = "#f9fafb"
quote = "#9ca3af"
quote_bar = "#4b5563"
table_border = "#374151"
table_stripe = "#1f2937"
body_font = "bundled"
heading_font = "bundled"
code_font = "bundled"
body_size_px = 16
code_size_px = 16
heading_scale_percent = 100
line_height_percent = 140
page_padding_px = 12
```

Every field is optional. A custom theme extends a built-in theme; `auto` is the
default. Omitted settings inherit from that built-in theme. In particular, a
theme that extends `light` inherits the light rendered palette. The example
above shows all settings, but its rendered colors are example overrides.

Interface, syntax, and Markdown source colors accept `#RRGGBB`, `default`, or
ANSI names such as `blue`, `light-cyan`, and `dark-gray`. Syntax and Markdown
source colors are independent of interface colors. Syntax colors also apply to
recognized code inside Markdown fences. Markdown source uses the terminal font.

Rendered colors accept only `#RRGGBB`. Each font setting accepts `bundled` or a
local TTF/OTF file path. Relative paths start at the directory of `config.toml`.
Missing and invalid files cause a configuration error. The default uses bundled
fonts for all three roles. Body and code sizes are pixels from 9 to 32. Heading
scale is a percentage from 50 to 200. Line height is a percentage from 100 to
400. Page padding is pixels from 0 to 128. The defaults are 16 pixels for body
and code, 100 percent heading scale, 140 percent line height, and 12 pixels
padding. Rendered font settings do not change the terminal font.
Each font path selects one face. A body font replaces the regular face. Bold and
italic faces remain bundled, though a variable face can provide bold weight.

Run `de theme` to preview the picker and Markdown sample. Custom names also work
with `--theme` and `DE_THEME`.

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
