# de

`de` is a tiny inline directory explorer for your shell. It shows the current
directory, lets you walk into or out of folders, and changes the shell's working
directory only when you confirm.

At normal terminal widths, the left pane is the directory you are currently
exploring and the right pane previews the highlighted destination. Below 58
columns, `de` collapses to a single pane instead of squeezing the listings.

It is intentionally not a file manager. Files are dimmed context; there are no
delete, rename, copy, edit, or open commands.

## Try it

```sh
cargo build --release
export PATH="$PWD/target/release:$PATH"
eval "$(command de init bash)" # use zsh here if appropriate
de
```

For fish:

```fish
command de init fish | source
```

Add the relevant initialization line to your shell profile once you decide to
keep it. The generated function captures the directory printed by the executable
and asks the parent shell to `cd` there; a child process cannot change its parent
shell's directory directly.

## Controls

| Key | Action |
| --- | --- |
| `Up` / `Down`, `j` / `k` | Select an entry |
| `PageUp` / `PageDown` | Jump one visible page through the entries |
| `Right`, `l`, `Tab` | Make the previewed directory current |
| `Left`, `h`, `Backspace` | Go to the parent directory |
| `/` | Filter entries in the current directory |
| `Enter` | Change to the directory currently displayed |
| `.` | Toggle hidden entries |
| `r` | Refresh |
| `Escape`, `q`, `Ctrl-C` | Cancel without changing directory |

The important distinction is that `Enter` accepts the directory in the header,
not the highlighted child. Use `Right` to explore and `Enter` when you have
arrived.

Filtering uses a case-insensitive substring match. Type after pressing `/`, use
the normal arrow and page keys to move through matches, and press `Backspace` to
edit the query. `Escape` clears an active filter first; press it again to cancel
`de`.

## Built with

- [Ratatui](https://ratatui.rs/) renders the interactive panes and layout.
- [Crossterm](https://github.com/crossterm-rs/crossterm) handles terminal input,
  raw mode, cursor movement, and colors.
- [Clap](https://docs.rs/clap/) parses arguments and generates the styled help,
  version, subcommand, and error output.

The small relative-coordinate backend in `src/backend.rs` keeps the picker
inline without requiring the terminal to answer an absolute cursor-position
query. This is useful in PTYs and layered terminal environments.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

The UI is rendered on stderr so stdout remains a clean selected-path protocol
for the shell wrapper. On Unix, path bytes are preserved by the executable; like
most command-substitution integrations, directories whose names end in newline
characters are outside the supported shell-wrapper boundary.
