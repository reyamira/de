use clap::builder::styling::{AnsiColor, Styles};
use clap::{Parser, Subcommand, ValueEnum};
use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{self, Clear, ClearType, disable_raw_mode, enable_raw_mode};
use de::backend::InlineBackend;
use de::{App, NavigationResult, TWO_PANE_MIN_WIDTH, render, resolve_start_path, shell_init};
use ratatui::Terminal;
use ratatui::layout::Rect;
use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

const PICKER_HELP: &str = "Picker controls:
  ↑/↓ or j/k         Select          PageUp/PageDown  Jump a page
  → or l/Tab         Open folder     ← or h/Backspace  Go to parent
  /                  Filter          . / r              Hidden / refresh
  Enter              Go here         Esc / q / Ctrl-C   Cancel

Run `de init --help` for shell setup.";

const SHELL_SETUP_HELP: &str = "Setup examples:
  Bash   eval \"$(command de init bash)\"
  Zsh    eval \"$(command de init zsh)\"
  Fish   command de init fish | source

Add the command for your shell to its startup file to make `de` available in
future sessions.";

const CLI_STYLES: Styles = Styles::styled()
    .header(AnsiColor::BrightCyan.on_default().bold())
    .usage(AnsiColor::BrightCyan.on_default().bold())
    .literal(AnsiColor::BrightCyan.on_default().bold())
    .placeholder(AnsiColor::BrightWhite.on_default())
    .error(AnsiColor::BrightRed.on_default().bold())
    .valid(AnsiColor::BrightGreen.on_default())
    .invalid(AnsiColor::BrightYellow.on_default().bold());

#[derive(Debug, Parser)]
#[command(
    name = "de",
    version,
    about = "Explore directories inline, then cd when you confirm",
    long_about = None,
    after_long_help = PICKER_HELP,
    styles = CLI_STYLES,
    disable_help_subcommand = true,
    args_conflicts_with_subcommands = true,
    subcommand_precedence_over_arg = true
)]
struct Cli {
    /// Directory to start exploring from
    #[arg(value_name = "DIRECTORY")]
    directory: Option<OsString>,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Print shell integration code
    #[command(
        long_about = "Print the shell function that lets de change its parent shell's directory.",
        after_long_help = SHELL_SETUP_HELP
    )]
    Init {
        /// Shell whose integration should be generated
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Shell {
    /// Bash
    Bash,
    /// Z shell
    Zsh,
    /// Fish
    Fish,
}

impl Shell {
    const fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("de: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Some(CliCommand::Init { shell }) => {
            let script = shell_init(shell.name()).expect("Clap limits shells to supported values");
            println!("{script}");
        }
        None => {
            if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
                return Err("interactive mode needs a terminal on stdin and stderr".into());
            }
            let start = resolve_start_path(cli.directory.as_deref())
                .map_err(|error| format!("cannot resolve start directory: {error}"))?;
            let app =
                App::new(start).map_err(|error| format!("cannot open start directory: {error}"))?;
            if let Some(path) = run_picker(app).map_err(|error| error.to_string())? {
                write_selected_path(&path).map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

fn run_picker(mut app: App) -> io::Result<Option<std::path::PathBuf>> {
    enable_raw_mode()?;
    let mut raw_mode = RawModeGuard {
        viewport_active: false,
    };
    execute!(io::stderr(), cursor::Hide)?;

    let (mut terminal_width, mut terminal_height) = terminal::size()?;
    let mut viewport_height = desired_viewport_height(&app, terminal_width, terminal_height);
    let backend = InlineBackend::new(io::stderr(), terminal_width, viewport_height)?;
    raw_mode.viewport_active = true;
    let mut terminal = Terminal::new(backend)?;

    let outcome: io::Result<Option<std::path::PathBuf>> = (|| {
        loop {
            terminal.draw(|frame| render(frame, &app))?;
            match event::read()? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    let page_rows = visible_entry_rows(&app, terminal_width, viewport_height);
                    match handle_key(&mut app, key, page_rows) {
                        NavigationResult::Continue => {
                            let desired =
                                desired_viewport_height(&app, terminal_width, terminal_height);
                            if desired != viewport_height {
                                terminal
                                    .backend_mut()
                                    .resize_viewport(terminal_width, desired)?;
                                terminal.resize(Rect::new(0, 0, terminal_width, desired))?;
                                viewport_height = desired;
                            }
                        }
                        NavigationResult::Accept(path) => break Ok(Some(path)),
                        NavigationResult::Cancel => break Ok(None),
                    }
                }
                Event::Resize(width, height) => {
                    terminal_width = width;
                    terminal_height = height;
                    viewport_height =
                        desired_viewport_height(&app, terminal_width, terminal_height);
                    terminal
                        .backend_mut()
                        .resize_viewport(width, viewport_height)?;
                    terminal.resize(Rect::new(0, 0, width, viewport_height))?;
                }
                _ => {}
            }
        }
    })();

    let cleanup = terminal.backend_mut().finish();
    drop(terminal);
    cleanup?;
    outcome
}

fn desired_viewport_height(app: &App, terminal_width: u16, terminal_height: u16) -> u16 {
    let status_row = usize::from(app.status().is_some());
    let current_rows = app.entries().len().saturating_add(status_row);
    let body_rows = if terminal_width >= TWO_PANE_MIN_WIDTH {
        let preview_rows = app
            .preview()
            .entries()
            .len()
            .saturating_add(usize::from(app.preview().message().is_some()));
        current_rows.max(preview_rows).saturating_add(1)
    } else {
        current_rows
    };
    let content_height = body_rows.saturating_add(2);
    let content_height = u16::try_from(content_height)
        .unwrap_or(u16::MAX)
        .clamp(3, 14);
    content_height.min(terminal_height.saturating_sub(1).max(1))
}

fn visible_entry_rows(app: &App, terminal_width: u16, viewport_height: u16) -> usize {
    let chrome_rows = if terminal_width >= TWO_PANE_MIN_WIDTH {
        3
    } else {
        2
    };
    usize::from(viewport_height.saturating_sub(chrome_rows))
        .saturating_sub(usize::from(app.status().is_some()))
        .max(1)
}

fn handle_key(app: &mut App, key: KeyEvent, page_rows: usize) -> NavigationResult {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return NavigationResult::Cancel;
    }

    if app.is_filtering() {
        match key.code {
            KeyCode::Up => app.move_up(),
            KeyCode::Down => app.move_down(),
            KeyCode::PageUp => app.page_up(page_rows),
            KeyCode::PageDown => app.page_down(page_rows),
            KeyCode::Home => app.move_first(),
            KeyCode::End => app.move_last(),
            KeyCode::Right | KeyCode::Tab => app.enter_selected(),
            KeyCode::Left => app.go_parent(),
            KeyCode::Backspace => app.pop_filter_char(),
            KeyCode::Enter => return app.accept(),
            KeyCode::Esc => app.clear_filter(),
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                app.push_filter_char(character);
            }
            _ => {}
        }
        return NavigationResult::Continue;
    }

    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.move_up(),
        KeyCode::Down | KeyCode::Char('j') => app.move_down(),
        KeyCode::PageUp => app.page_up(page_rows),
        KeyCode::PageDown => app.page_down(page_rows),
        KeyCode::Home | KeyCode::Char('g') => app.move_first(),
        KeyCode::End | KeyCode::Char('G') => app.move_last(),
        KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => app.enter_selected(),
        KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => app.go_parent(),
        KeyCode::Char('/') => app.begin_filter(),
        KeyCode::Char('.') => app.toggle_hidden(),
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Enter => return app.accept(),
        KeyCode::Esc | KeyCode::Char('q') => return NavigationResult::Cancel,
        _ => {}
    }
    NavigationResult::Continue
}

struct RawModeGuard {
    viewport_active: bool,
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        if self.viewport_active {
            let _ = execute!(
                io::stderr(),
                Clear(ClearType::FromCursorDown),
                cursor::MoveToColumn(0),
                cursor::Show
            );
        } else {
            let _ = execute!(io::stderr(), cursor::Show, cursor::MoveToColumn(0));
        }
    }
}

#[cfg(unix)]
fn write_selected_path(path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let mut stdout = io::stdout().lock();
    stdout.write_all(path.as_os_str().as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

#[cfg(not(unix))]
fn write_selected_path(path: &Path) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{}", path.display())?;
    stdout.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn parses_a_start_directory() {
        let cli = Cli::try_parse_from(["de", "../somewhere"]).unwrap();
        assert_eq!(cli.directory, Some(OsString::from("../somewhere")));
        assert!(cli.command.is_none());
    }

    #[test]
    fn parses_a_supported_shell() {
        let cli = Cli::try_parse_from(["de", "init", "fish"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Init { shell: Shell::Fish })
        ));
    }

    #[test]
    fn init_requires_exactly_one_supported_shell() {
        assert_eq!(
            Cli::try_parse_from(["de", "init"]).unwrap_err().kind(),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            Cli::try_parse_from(["de", "init", "powershell"])
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidValue
        );
    }

    #[test]
    fn short_help_stays_focused_on_the_clap_interface() {
        let help = Cli::try_parse_from(["de", "-h"]).unwrap_err().to_string();
        assert!(help.contains("Usage:"));
        assert!(help.contains("Commands:"));
        assert!(help.contains("DIRECTORY"));
        assert!(!help.contains("Picker controls:"));
        assert!(!help.contains("Setup examples:"));
    }

    #[test]
    fn long_help_adds_picker_controls_and_routes_shell_setup() {
        let help = Cli::try_parse_from(["de", "--help"])
            .unwrap_err()
            .to_string();
        assert!(help.contains("Picker controls:"));
        assert!(help.contains("PageUp/PageDown"));
        assert!(help.contains("de init --help"));
        assert!(!help.contains("eval \"$(command de init bash)\""));
    }

    #[test]
    fn init_long_help_owns_shell_setup_examples() {
        let help = Cli::try_parse_from(["de", "init", "--help"])
            .unwrap_err()
            .to_string();
        assert!(help.contains("Possible values:"));
        assert!(help.contains("bash"));
        assert!(help.contains("zsh"));
        assert!(help.contains("fish"));
        assert!(help.contains("Setup examples:"));
        assert!(help.contains("command de init fish | source"));
    }

    #[test]
    fn slash_enters_filter_mode_and_escape_clears_before_canceling() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("source")).unwrap();
        fs::write(temp.path().join("notes.txt"), "notes").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();

        let slash = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
        assert_eq!(handle_key(&mut app, slash, 5), NavigationResult::Continue);
        assert!(app.is_filtering());

        let character = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
        assert_eq!(
            handle_key(&mut app, character, 5),
            NavigationResult::Continue
        );
        assert_eq!(app.filter_query(), "s");
        assert_eq!(app.entries().len(), 2);

        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(handle_key(&mut app, escape, 5), NavigationResult::Continue);
        assert!(!app.is_filtering());
        assert!(app.filter_query().is_empty());
        assert_eq!(app.entries().len(), 2);
        assert_eq!(handle_key(&mut app, escape, 5), NavigationResult::Cancel);
    }
}
