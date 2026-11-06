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
