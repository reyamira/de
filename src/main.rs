use clap::builder::styling::{AnsiColor, Styles};
use clap::{Parser, Subcommand, ValueEnum};
use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{self, Clear, ClearType, disable_raw_mode, enable_raw_mode};
use de::backend::InlineBackend;
use de::{
    App, Config, NavigationResult, THEME_ENV, TWO_PANE_MIN_WIDTH, Theme, create_custom_theme,
    render, render_theme_preview, resolve_start_path, save_theme, shell_init,
};
use ratatui::Terminal;
use ratatui::layout::Rect;
use std::env;
use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

const PICKER_HELP: &str = "Picker controls:
  ↑/↓ or j/k         Select          PageUp/PageDown  Jump a page
  → or l/Tab         Open folder     ← or h/Backspace  Go to parent
  /                  Filter          s                  Name / modified
  Shift+S            Ascending / descending
  o                  Open file with its default application
  . / r              Hidden / refresh
  Enter              Go here         Esc / q / Ctrl-C   Cancel

Run `de theme` to preview and save a color theme.
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
    /// Use a built-in or custom color theme for this invocation
    #[arg(long, value_name = "THEME")]
    theme: Option<String>,

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

    /// Preview, save, or create themes
    Theme {
        #[command(subcommand)]
        command: Option<ThemeCommand>,
    },
}

#[derive(Debug, Subcommand)]
enum ThemeCommand {
    /// Add an editable custom theme template to config.toml
    Create {
        /// Name for the custom theme
        name: String,
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
    let Cli {
        theme,
        directory,
        command,
    } = cli;
    match command {
        Some(CliCommand::Init { shell }) => {
            let script = shell_init(shell.name()).expect("Clap limits shells to supported values");
            println!("{script}");
        }
        Some(CliCommand::Theme {
            command: Some(ThemeCommand::Create { name }),
        }) => {
            let path = create_custom_theme(&name)
                .map_err(|error| format!("cannot create theme {name:?}: {error}"))?;
            eprintln!(
                "Created theme {name} in {}. Edit its colors, then run `de theme` to preview it.",
                path.display()
            );
        }
        Some(CliCommand::Theme { command: None }) => {
            require_terminal()?;
            let catalog =
                Config::load().map_err(|error| format!("cannot load theme config: {error}"))?;
            let start = resolve_start_path(None)
                .map_err(|error| format!("cannot resolve current directory: {error}"))?;
            let mut app = App::new(start)
                .map_err(|error| format!("cannot open current directory: {error}"))?;
            app.set_theme(resolve_theme(theme, &catalog)?);
            app.set_display_settings(catalog.display().clone());
            if let Some(theme) =
                run_theme_picker(app, &catalog).map_err(|error| error.to_string())?
            {
                let path = save_theme(theme.name())
                    .map_err(|error| format!("cannot save theme: {error}"))?;
                eprintln!("Saved theme {} to {}", theme.name(), path.display());
            }
        }
        None => {
            require_terminal()?;
            let catalog =
                Config::load().map_err(|error| format!("cannot load theme config: {error}"))?;
            let start = resolve_start_path(directory.as_deref())
                .map_err(|error| format!("cannot resolve start directory: {error}"))?;
            let mut app =
                App::new(start).map_err(|error| format!("cannot open start directory: {error}"))?;
            app.set_theme(resolve_theme(theme, &catalog)?);
            app.set_display_settings(catalog.display().clone());
            if let Some(action) = run_picker(app).map_err(|error| error.to_string())? {
                match action {
                    PickerAction::ChangeDirectory(path) => {
                        write_selected_path(&path).map_err(|error| error.to_string())?;
                    }
                    PickerAction::OpenFile(path) => {
                        open_with_default_app(&path)
                            .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn require_terminal() -> Result<(), String> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err("interactive mode needs a terminal on stdin and stderr".into());
    }
    Ok(())
}

fn resolve_theme(override_theme: Option<String>, catalog: &Config) -> Result<Theme, String> {
    let requested = if let Some(theme) = override_theme {
        theme
    } else {
        match env::var(THEME_ENV) {
            Ok(value) => value,
            Err(env::VarError::NotPresent) => catalog.saved_theme().unwrap_or("auto").to_owned(),
            Err(env::VarError::NotUnicode(_)) => {
                return Err(format!("{THEME_ENV} must be valid UTF-8"));
            }
        }
    };

    catalog.find(&requested).cloned().ok_or_else(|| {
        format!(
            "unknown theme {requested:?}; available themes: {}",
            catalog.names().collect::<Vec<_>>().join(", ")
        )
    })
}

enum InlineResult<T> {
    Continue,
    Accept(T),
    Cancel,
}

enum PickerAction {
    ChangeDirectory(std::path::PathBuf),
    OpenFile(std::path::PathBuf),
}

fn run_picker(app: App) -> io::Result<Option<PickerAction>> {
    run_inline(app, render, |app, key, page_rows| {
        match handle_key(app, key, page_rows) {
            NavigationResult::Continue => InlineResult::Continue,
            NavigationResult::Accept(path) => {
                InlineResult::Accept(PickerAction::ChangeDirectory(path))
            }
            NavigationResult::Open(path) => InlineResult::Accept(PickerAction::OpenFile(path)),
            NavigationResult::Cancel => InlineResult::Cancel,
        }
    })
}

fn run_theme_picker(app: App, catalog: &Config) -> io::Result<Option<Theme>> {
    run_inline(app, render_theme_preview, |app, key, _| {
        handle_theme_key(app, key, catalog)
    })
}

fn run_inline<T>(
    mut app: App,
    mut draw: impl FnMut(&mut ratatui::Frame<'_>, &App),
    mut handle: impl FnMut(&mut App, KeyEvent, usize) -> InlineResult<T>,
) -> io::Result<Option<T>> {
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

    let outcome: io::Result<Option<T>> = (|| {
        loop {
            terminal.draw(|frame| draw(frame, &app))?;
            match event::read()? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    let page_rows = visible_entry_rows(&app, terminal_width, viewport_height);
                    match handle(&mut app, key, page_rows) {
                        InlineResult::Continue => {
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
                        InlineResult::Accept(value) => break Ok(Some(value)),
                        InlineResult::Cancel => break Ok(None),
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

fn handle_theme_key(app: &mut App, key: KeyEvent, catalog: &Config) -> InlineResult<Theme> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return InlineResult::Cancel;
    }

    match key.code {
        KeyCode::Left | KeyCode::Up | KeyCode::Char('h') | KeyCode::Char('k') => {
            app.set_theme(catalog.previous(app.theme().name()));
        }
        KeyCode::Right | KeyCode::Down | KeyCode::Tab | KeyCode::Char('l') | KeyCode::Char('j') => {
            app.set_theme(catalog.next(app.theme().name()))
        }
        KeyCode::Enter => return InlineResult::Accept(app.theme().clone()),
        KeyCode::Esc | KeyCode::Char('q') => return InlineResult::Cancel,
        _ => {}
    }
    InlineResult::Continue
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
        KeyCode::Char('s') => app.cycle_sort(),
        KeyCode::Char('S') => app.toggle_sort_direction(),
        KeyCode::Char('.') => app.toggle_hidden(),
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Char('o') => return app.open_selected(),
        KeyCode::Enter => return app.accept(),
        KeyCode::Esc | KeyCode::Char('q') => return NavigationResult::Cancel,
        _ => {}
    }
    NavigationResult::Continue
}

fn open_with_default_app(path: &Path) -> io::Result<()> {
    let mut command = default_open_command(path)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn default_open_command(path: &Path) -> io::Result<Command> {
    let mut command = Command::new("open");
    command.arg(path);
    Ok(command)
}

#[cfg(target_os = "windows")]
fn default_open_command(path: &Path) -> io::Result<Command> {
    let mut command = Command::new("cmd");
    command.args(["/C", "start", ""]).arg(path);
    Ok(command)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn default_open_command(path: &Path) -> io::Result<Command> {
    let mut command = Command::new("xdg-open");
    command.arg(path);
    Ok(command)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn default_open_command(_path: &Path) -> io::Result<Command> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "opening files is not supported on this platform",
    ))
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
        assert_eq!(cli.theme, None);
        assert!(cli.command.is_none());
    }

    #[test]
    fn parses_a_one_time_theme_and_the_theme_selector() {
        let cli = Cli::try_parse_from(["de", "--theme", "dark", "../somewhere"]).unwrap();
        assert_eq!(cli.theme.as_deref(), Some("dark"));
        assert!(cli.command.is_none());

        let cli = Cli::try_parse_from(["de", "theme"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Theme { command: None })
        ));

        let cli = Cli::try_parse_from(["de", "theme", "create", "midnight"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Theme {
                command: Some(ThemeCommand::Create { name })
            }) if name == "midnight"
        ));
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
        assert!(help.contains("theme"));
        assert!(help.contains("--theme <THEME>"));
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
        assert!(help.contains("Open file with its default application"));
        assert!(help.contains("de theme"));
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

    #[test]
    fn lowercase_s_changes_criterion_and_uppercase_s_changes_direction() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("source")).unwrap();
        fs::write(temp.path().join("notes.txt"), "notes").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();

        let lowercase = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
        assert_eq!(
            handle_key(&mut app, lowercase, 5),
            NavigationResult::Continue
        );
        assert_eq!(app.sort_mode().label(), "time");
        assert_eq!(app.sort_direction().symbol(), "↑");

        let uppercase = KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT);
        assert_eq!(
            handle_key(&mut app, uppercase, 5),
            NavigationResult::Continue
        );
        assert_eq!(app.sort_mode().label(), "time");
        assert_eq!(app.sort_direction().symbol(), "↓");
    }

    #[test]
    fn lowercase_o_opens_the_highlighted_file_and_enter_still_accepts_the_directory() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("source")).unwrap();
        fs::write(temp.path().join("notes.txt"), "notes").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.move_last();

        let open = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(
            handle_key(&mut app, open, 5),
            NavigationResult::Open(temp.path().join("notes.txt"))
        );

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            handle_key(&mut app, enter, 5),
            NavigationResult::Accept(temp.path().to_path_buf())
        );
    }

    #[test]
    fn o_remains_filter_text_while_filtering() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("notes.txt"), "notes").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_filter();

        let open = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(handle_key(&mut app, open, 5), NavigationResult::Continue);
        assert_eq!(app.filter_query(), "o");
    }

    #[test]
    fn default_opener_receives_the_file_path_as_one_argument() {
        let path = Path::new("a file with spaces.txt");
        let command = default_open_command(path).unwrap();
        let args = command.get_args().collect::<Vec<_>>();

        #[cfg(target_os = "windows")]
        assert_eq!(args, ["/C", "start", "", "a file with spaces.txt"]);

        #[cfg(not(target_os = "windows"))]
        assert_eq!(args, ["a file with spaces.txt"]);
    }

    #[test]
    fn theme_picker_cycles_both_ways_and_accepts_the_previewed_theme() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("source")).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let catalog = Config::built_ins();
        assert_eq!(app.theme().name(), "auto");

        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        assert!(matches!(
            handle_theme_key(&mut app, right, &catalog),
            InlineResult::Continue
        ));
        assert_eq!(app.theme().name(), "light");

        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        assert!(matches!(
            handle_theme_key(&mut app, left, &catalog),
            InlineResult::Continue
        ));
        assert_eq!(app.theme().name(), "auto");

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        match handle_theme_key(&mut app, enter, &catalog) {
            InlineResult::Accept(theme) => assert_eq!(theme.name(), "auto"),
            _ => panic!("Enter should accept the previewed theme"),
        }
    }
}
