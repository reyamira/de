# Development

Return to the [README](../README.md) for installation and usage. See the [release checklist](releasing.md) for publication.

## Built with

- [Ratatui](https://ratatui.rs/) renders the interactive panes and layout.
- [Crossterm](https://github.com/crossterm-rs/crossterm) handles raw mode,
  cursor movement, and colors.
- [Termina](https://docs.rs/termina/) reads keyboard, mouse, resize, and
  cursor-position events.
- [Syntect](https://docs.rs/syntect/) highlights source files.
- [franken_markdown](https://crates.io/crates/franken_markdown) renders Markdown
  to SVG. [resvg](https://docs.rs/resvg/) rasterizes the visible page.
- [Clap](https://docs.rs/clap/) parses arguments and generates the styled help,
  version, subcommand, and error output.
- [toml_edit](https://docs.rs/toml_edit/) reads and updates configuration while
  retaining the user's formatting and comments.

The small relative-coordinate backend in `src/backend.rs` keeps the picker inline.
Keyboard navigation does not require cursor-position queries.
Mouse controls use an optional query to locate the viewer within the terminal.

## Checks

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
small public artifact embedded in the README.

The UI is rendered on stderr so stdout remains a clean selected-path protocol
for the shell wrapper. On Unix, path bytes are preserved by the executable; like
most command-substitution integrations, directories whose names end in newline
characters are outside the supported shell-wrapper boundary.

