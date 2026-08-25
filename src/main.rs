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

const AFTER_HELP: &str = "Navigation:
  ↑/↓, j/k            Select an entry
  →, l, Tab           Enter the directory shown in the right pane
  ←, h, Backspace     Go to the parent directory
  Enter               Change the shell to the current directory
  . / r               Toggle hidden entries / refresh
  Esc, q, Ctrl-C      Cancel without changing directory

Shell setup:
  Bash:  eval \"$(command de init bash)\"
  Zsh:   eval \"$(command de init zsh)\"
  Fish:  command de init fish | source";

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
    after_help = AFTER_HELP,
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
    Init {
        /// Shell whose integration should be generated
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Shell {
    Bash,
    Zsh,
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
                    match handle_key(&mut app, key) {
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

fn handle_key(app: &mut App, key: KeyEvent) -> NavigationResult {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return NavigationResult::Cancel;
    }

    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.move_up(),
        KeyCode::Down | KeyCode::Char('j') => app.move_down(),
        KeyCode::Home | KeyCode::Char('g') => app.move_first(),
        KeyCode::End | KeyCode::Char('G') => app.move_last(),
        KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => app.enter_selected(),
        KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => app.go_parent(),
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
    fn generated_help_includes_commands_and_navigation() {
        let help = Cli::try_parse_from(["de", "--help"])
            .unwrap_err()
            .to_string();
        assert!(help.contains("Usage:"));
        assert!(help.contains("Commands:"));
        assert!(help.contains("Navigation:"));
        assert!(help.contains("DIRECTORY"));
    }
}
