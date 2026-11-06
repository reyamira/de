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

## What's new in v0.5.0

- `de theme edit` provides interactive editing, inherited values, input guidance,
  and safe save/cancel behavior. Built-in themes save as named custom copies.
- Separate palettes control interface, syntax, Markdown source, and rendered colors.
- Rendered Markdown supports local body, heading, and code fonts, configurable sizes,
  heading scale, line spacing, and page padding.
- Complete Browser, Source, and Rendered Markdown samples demonstrate every theme role.
  Tab cycles the editor's right panel without hiding the settings.
- Long values and theme names have visible carets and horizontal scrolling.

See the [v0.5.0 release notes](https://github.com/reyamira/de/releases/tag/v0.5.0)
and [release history](https://github.com/reyamira/de/releases) for changes between versions.

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
`de`, this README, the MIT license, and grammar licenses and attribution files.
SHA-256 checksums accompany the archives. Extract the archive, then place `de`
somewhere on your `PATH`.

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
| `R` (plain text viewer) | Toggle raw and reading layouts |
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

The right pane previews Markdown source, JSON, source code, plain text, and
supported images. Press `v` for the full-width viewer. For Markdown, press `m`
to switch between the rendered page and the same highlighted source. Use arrows
or `j`/`k` to scroll. Press `Escape` or `q` to return to the same selection.

The [viewer guide](docs/viewer.md) covers supported formats, controls, graphics
requirements, Markdown rendering, and size limits.

### Filtering and sorting

Filtering uses a case-insensitive substring match. Type after pressing `/`, use
the normal arrow and page keys to move through matches, and press `Backspace` to
edit the query. `Escape` clears an active filter first; press it again to cancel
`de`.

Name sorting is case-insensitive and alphabetical. Sort criterion and direction
are independent, so modification time can show either oldest or newest first.
Directories remain grouped above files in every mode, and the selected sort
carries into the preview pane and navigated directories.

## Configuration

Run `de config` to change settings with a live preview, or run `de theme` to
choose a theme. Run `de theme edit` to edit a theme with an interactive preview.
Press `w` in the picker to save the current hidden and sort settings as defaults.

In `de theme`, use the arrows or `h`/`j`/`k`/`l` to change themes.
Press `Tab` to cycle through the browser, Markdown source, and rendered Markdown samples.
In a Markdown sample, press `m` to switch between source and rendered views.
Use Page Up and Page Down to see examples below the visible area.
Press Enter to save the selected theme, or Esc to cancel.
The sample uses no file in your workspace.

In `de theme edit [NAME]`, use `Up`/`Down` to select a field and `Enter` to
edit it. Press `r` to reset it to the inherited value. An empty edit accepts
inheritance. Inherited fields show their actual values; accepting an unchanged
value keeps inheritance.

Press `Tab` to cycle the right panel through Browser, Markdown source, and
rendered Markdown. The fields keep focus. Use `PageUp`/`PageDown` to see
examples below the visible area. Narrow terminals hide the previews. Press `s`
to save, or `Escape` to cancel. The [configuration guide](docs/configuration.md#themes)
has more details.

The [configuration guide](docs/configuration.md) covers file locations, editing,
defaults, themes, and date and time formats.

## Development and releases

See the [development guide](docs/development.md) for dependencies, checks, demo
replay, and the stdout protocol. Maintainers can use the
[release checklist](docs/releasing.md).
