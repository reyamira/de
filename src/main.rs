use clap::builder::styling::{AnsiColor, Styles};
use clap::{Parser, Subcommand, ValueEnum};
use crossterm::cursor;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{self, Clear, ClearType, disable_raw_mode, enable_raw_mode};
use de::backend::InlineBackend;
use de::{
    App, BrowseDefaults, CONFIG_PICKER_MIN_HEIGHT, Config, DisplaySettings, NavigationResult,
    SettingsPicker, THEME_ENV, TWO_PANE_MIN_WIDTH, Theme, create_custom_theme, ensure_config_file,
    preview_image_area, render, render_config_picker, render_theme_preview, resolve_start_path,
    save_defaults, save_settings, save_theme, shell_init, theme_config_path, viewer_body,
    viewer_page_rows, viewer_scroll_max, viewer_scrollbar,
};
use ratatui::Terminal;
use ratatui::layout::Rect;
use std::env;
use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;
use termina::escape::csi::{Csi, Cursor};
use termina::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers as KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use termina::{EventReader, PlatformTerminal, Terminal as _};

const PICKER_HELP: &str = "Picker controls:
  ↑/↓ or j/k         Select          PageUp/PageDown  Jump a page
  → or l/Tab         Open folder     ← or h/Backspace  Go to parent
  /                  Filter          s                  Name / modified
  Shift+S            Ascending / descending
  v                  View file inline   Esc / q           Return to picker
  m                  Toggle Markdown source / rendered inside the viewer
  R                  Toggle plain-text reading / raw inside the viewer
  o                  Open file with its default application
  . / r              Hidden / refresh
  w                  Save hidden and sort settings as defaults
  Enter              Go here         Esc / q / Ctrl-C   Cancel

Run `de theme` to preview and save a color theme.
Run `de config` to change settings, or `de config edit` to open the file.
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

    /// List loaded syntax grammars and their file extensions
    #[command(
        long_about = "List loaded syntax grammars and their file extensions, one grammar per line. \
Each line contains the grammar name, a tab, and comma-separated extensions. \
Grammars without extensions show (none)."
    )]
    Languages,

    /// Preview, save, or create themes
    Theme {
        #[command(subcommand)]
        command: Option<ThemeCommand>,
    },

    /// Change settings, or locate and edit config.toml
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommand>,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Print the path to config.toml
    Path,

    /// Open config.toml in $VISUAL or $EDITOR, creating it if needed
    Edit,
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
        Some(CliCommand::Languages) => {
            let mut definitions: Vec<_> = de::document::syntax_definitions().collect();
            definitions.sort_unstable();
            for (name, extensions) in definitions {
                let mut extensions: Vec<_> = extensions.iter().map(String::as_str).collect();
                extensions.sort_unstable();
                let extensions = if extensions.is_empty() {
                    "(none)".to_owned()
                } else {
                    extensions.join(", ")
                };
                println!("{name}\t{extensions}");
            }
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
        Some(CliCommand::Config {
            command: Some(ConfigCommand::Path),
        }) => {
            let path =
                theme_config_path().map_err(|error| format!("cannot locate config: {error}"))?;
            println!("{}", path.display());
        }
        Some(CliCommand::Config { command: None }) => {
            require_terminal()?;
            let catalog = Config::load().map_err(|error| format!("cannot load config: {error}"))?;
            let start = resolve_start_path(None)
                .map_err(|error| format!("cannot resolve current directory: {error}"))?;
            let mut app = App::with_defaults(start, catalog.defaults())
                .map_err(|error| format!("cannot open current directory: {error}"))?;
            app.set_theme(resolve_theme(theme, &catalog)?);
            app.set_display_settings(catalog.display().clone());
            if let Some((defaults, display)) =
                run_config_picker(app).map_err(|error| error.to_string())?
            {
                let path = save_settings(defaults, &display)
                    .map_err(|error| format!("cannot save settings: {error}"))?;
                eprintln!("Saved settings to {}", path.display());
            }
        }
        Some(CliCommand::Config {
            command: Some(ConfigCommand::Edit),
        }) => {
            let path =
                ensure_config_file().map_err(|error| format!("cannot create config: {error}"))?;
            let status = editor_command(&path)?
                .status()
                .map_err(|error| format!("cannot start editor: {error}"))?;
            if !status.success() {
                return Err(format!("editor exited with {status}"));
            }
            Config::load().map_err(|error| format!("{} has an error: {error}", path.display()))?;
        }
        Some(CliCommand::Theme { command: None }) => {
            require_terminal()?;
            let catalog = Config::load().map_err(|error| format!("cannot load config: {error}"))?;
            let start = resolve_start_path(None)
                .map_err(|error| format!("cannot resolve current directory: {error}"))?;
            let mut app = App::with_defaults(start, catalog.defaults())
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
            let catalog = Config::load().map_err(|error| format!("cannot load config: {error}"))?;
            let start = resolve_start_path(directory.as_deref())
                .map_err(|error| format!("cannot resolve start directory: {error}"))?;
            let mut app = App::with_defaults(start, catalog.defaults())
                .map_err(|error| format!("cannot open start directory: {error}"))?;
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
    run_inline(
        app,
        (),
        0,
        true,
        |frame, app, ()| render(frame, app),
        |app, (), key, page_rows| match handle_key(app, key, page_rows) {
            NavigationResult::Continue => InlineResult::Continue,
            NavigationResult::Accept(path) => {
                InlineResult::Accept(PickerAction::ChangeDirectory(path))
            }
            NavigationResult::Open(path) => InlineResult::Accept(PickerAction::OpenFile(path)),
            NavigationResult::Cancel => InlineResult::Cancel,
        },
    )
}

fn run_theme_picker(app: App, catalog: &Config) -> io::Result<Option<Theme>> {
    run_inline(
        app,
        (),
        0,
        true,
        |frame, app, ()| render_theme_preview(frame, app),
        |app, (), key, _| handle_theme_key(app, key, catalog),
    )
}

/// Returns the chosen settings on `Enter`, for the caller to save.
fn run_config_picker(app: App) -> io::Result<Option<(BrowseDefaults, DisplaySettings)>> {
    let picker = SettingsPicker::new(app.display_settings());
    run_inline(
        app,
        picker,
        CONFIG_PICKER_MIN_HEIGHT,
        false,
        render_config_picker,
        |app, picker, key, _| handle_config_key(app, picker, key),
    )
}

/// Drive an inline view. `state` is extra per-view data shared by `draw` and
/// `handle`; `min_height` keeps views whose content is not the entry list
/// from being sized to a short directory.
fn run_inline<S, T>(
    mut app: App,
    mut state: S,
    min_height: u16,
    images: bool,
    mut draw: impl FnMut(&mut ratatui::Frame<'_>, &App, &S),
    mut handle: impl FnMut(&mut App, &mut S, KeyEvent, usize) -> InlineResult<T>,
) -> io::Result<Option<T>> {
    let input_terminal = PlatformTerminal::new()?;
    let reader = input_terminal.event_reader();
    enable_raw_mode()?;
    let mut raw_mode = RawModeGuard {
        viewport_active: false,
        mouse_capture: false,
    };
    execute!(io::stderr(), cursor::Hide)?;

    let (mut terminal_width, mut terminal_height) = terminal::size()?;
    let mut viewport_height =
        desired_viewport_height(&app, terminal_width, terminal_height, min_height);
    let backend = InlineBackend::new(io::stderr(), terminal_width, viewport_height)?;
    raw_mode.viewport_active = true;
    let mut terminal = Terminal::new(backend)?;

    let outcome: io::Result<Option<T>> = (|| {
        let mut redraw = true;
        let mut viewer_active = false;
        let mut drag = None;
        let mut origin = None;
        let mut origin_supported = true;
        loop {
            redraw |= app.poll_preview();
            redraw |= app.poll_markdown();
            if redraw {
                let viewing = app.is_viewing();
                if viewer_active && !viewing {
                    drag = None;
                    set_mouse_capture(&mut raw_mode.mouse_capture, false, &mut io::stderr())?;
                    viewer_active = false;
                }
                let desired =
                    desired_viewport_height(&app, terminal_width, terminal_height, min_height);
                let mut measure_origin = viewing && !viewer_active;
                if desired != viewport_height {
                    terminal
                        .backend_mut()
                        .resize_viewport(terminal_width, desired)?;
                    terminal.resize(Rect::new(0, 0, terminal_width, desired))?;
                    viewport_height = desired;
                    drag = None;
                    measure_origin |= viewing;
                }
                if measure_origin {
                    origin = if origin_supported {
                        terminal.backend_mut().request_origin()?;
                        let result =
                            read_origin(&reader, terminal_width, terminal_height, viewport_height)?;
                        resolve_origin(result, &mut origin_supported)
                    } else {
                        None
                    };
                }
                set_mouse_capture(
                    &mut raw_mode.mouse_capture,
                    viewing && origin.is_some(),
                    &mut io::stderr(),
                )?;
                viewer_active = viewing;
                let area = Rect::new(0, 0, terminal_width, viewport_height);
                if app.is_viewing() {
                    app.set_viewer_scroll(app.viewer_scroll().min(viewer_scroll_max(&app, area)));
                }
                if app.is_rendered_markdown_view() {
                    let body = viewer_body(area);
                    let (cell_width, cell_height) = terminal_cell_pixels();
                    app.update_markdown_view(
                        u32::from(body.width) * cell_width,
                        u32::from(body.height) * cell_height,
                        cell_height,
                    );
                }
                terminal.draw(|frame| draw(frame, &app, &state))?;
                redraw = false;
            }
            let area = Rect::new(0, 0, terminal_width, viewport_height);
            let image = if images {
                app.markdown_image()
                    .or_else(|| {
                        (!app.is_rendered_markdown_view())
                            .then(|| app.preview().image())
                            .flatten()
                    })
                    .zip(preview_image_area(&app, area))
            } else {
                None
            };
            terminal.backend_mut().draw_image(image)?;

            // Read input between transfer batches, and wake for prepared images.
            if terminal.backend().image_pending() {
                if !reader.poll(Some(Duration::ZERO), |_| true)? {
                    continue;
                }
            } else if (app.image_pending() || app.markdown_pending())
                && !reader.poll(Some(Duration::from_millis(16)), |_| true)?
            {
                continue;
            }
            match reader.read(|_| true)? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    let page_rows = if app.is_viewing() {
                        viewer_page_rows(area)
                    } else {
                        visible_entry_rows(&app, terminal_width, viewport_height)
                    };
                    match handle(&mut app, &mut state, key, page_rows) {
                        InlineResult::Continue => {
                            if !app.is_viewing() || matches!(key.code, KeyCode::Char('m' | 'R')) {
                                drag = None;
                            }
                            redraw = true;
                        }
                        InlineResult::Accept(value) => break Ok(Some(value)),
                        InlineResult::Cancel => break Ok(None),
                    }
                }
                Event::Mouse(mouse) if viewer_active => {
                    redraw |= handle_viewer_mouse(&mut app, area, origin, mouse, &mut drag);
                }
                Event::WindowResized(size) => {
                    let (width, height) = (size.cols, size.rows);
                    set_mouse_capture(&mut raw_mode.mouse_capture, false, &mut io::stderr())?;
                    drag = None;
                    terminal_width = width;
                    terminal_height = height;
                    viewport_height = desired_viewport_height(&app, width, height, min_height);
                    terminal
                        .backend_mut()
                        .resize_viewport(width, viewport_height)?;
                    terminal.resize(Rect::new(0, 0, width, viewport_height))?;
                    origin = None;
                    if app.is_viewing() && origin_supported {
                        terminal.backend_mut().request_origin()?;
                        let result = read_origin(&reader, width, height, viewport_height)?;
                        origin = resolve_origin(result, &mut origin_supported);
                    }
                    set_mouse_capture(
                        &mut raw_mode.mouse_capture,
                        app.is_viewing() && origin.is_some(),
                        &mut io::stderr(),
                    )?;
                    redraw = true;
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

/// Wait briefly for a cursor report without consuming unrelated input.
fn read_origin(
    reader: &EventReader,
    width: u16,
    height: u16,
    viewport_height: u16,
) -> io::Result<Option<Option<u16>>> {
    let is_report = |event: &Event| {
        matches!(
            event,
            Event::Csi(Csi::Cursor(Cursor::ActivePositionReport { .. }))
        )
    };
    let deadline = std::time::Instant::now() + Duration::from_millis(100);
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        if reader.poll(Some(remaining), is_report)?
            && let Event::Csi(Csi::Cursor(Cursor::ActivePositionReport { line, col })) =
                reader.read(is_report)?
        {
            let row = line.get_zero_based();
            return Ok(Some(
                (col.get_zero_based() == 0
                    && width > 0
                    && viewport_height <= height
                    && row <= height - viewport_height)
                    .then_some(row),
            ));
        }
    }
    Ok(None)
}

// An invalid report can result from a queued resize; only a timeout disables
// future queries, because a late reply could otherwise be mistaken for a new one.
fn resolve_origin(report: Option<Option<u16>>, supported: &mut bool) -> Option<u16> {
    match report {
        Some(origin) => origin,
        None => {
            *supported = false;
            None
        }
    }
}

fn set_mouse_capture(active: &mut bool, wanted: bool, writer: &mut impl Write) -> io::Result<()> {
    if wanted && !*active {
        *active = true;
        execute!(writer, EnableMouseCapture)?;
    } else if !wanted && *active {
        execute!(writer, DisableMouseCapture)?;
        *active = false;
    }
    Ok(())
}

fn handle_viewer_mouse(
    app: &mut App,
    area: Rect,
    origin: Option<u16>,
    mouse: MouseEvent,
    drag: &mut Option<u16>,
) -> bool {
    if !app.is_viewing() {
        *drag = None;
        return false;
    }
    if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
        *drag = None;
        return false;
    }
    let Some(origin) = origin else {
        *drag = None;
        return false;
    };
    let row = mouse.row.saturating_sub(origin);
    let inside = mouse.row >= origin && row < area.height && mouse.column < area.width;
    let container = if area.height < 3 {
        area
    } else {
        Rect::new(area.x, area.y + 1, area.width, area.height - 2)
    };
    let point = ratatui::layout::Position::new(mouse.column, row);
    match mouse.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            if inside && container.contains(point) =>
        {
            let step = if app.is_rendered_markdown_view() {
                app.markdown_scroll_step()
            } else {
                1
            }
            .saturating_mul(3);
            let current = app.viewer_scroll();
            let maximum = viewer_scroll_max(app, area);
            let next = if mouse.kind == MouseEventKind::ScrollUp {
                current.saturating_sub(step)
            } else {
                current.saturating_add(step)
            }
            .min(maximum);
            app.set_viewer_scroll(next);
            next != current
        }
        MouseEventKind::Down(MouseButton::Left) if inside => {
            if let Some(bar) = viewer_scrollbar(app, area)
                && bar.track.contains(point)
            {
                if bar.thumb.contains(point) {
                    *drag = Some(row - bar.thumb.y);
                    return false;
                }
                let grab = bar.thumb.height / 2;
                *drag = Some(grab);
                let next = bar.scroll_at(row, grab);
                let changed = next != app.viewer_scroll();
                app.set_viewer_scroll(next);
                return changed;
            }
            false
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(grab) = *drag
                && let Some(bar) = viewer_scrollbar(app, area)
            {
                let next = bar.scroll_at(row, grab);
                let changed = next != app.viewer_scroll();
                app.set_viewer_scroll(next);
                return changed;
            }
            false
        }
        _ => false,
    }
}

fn terminal_cell_pixels() -> (u32, u32) {
    match terminal::window_size() {
        Ok(size)
            if size.columns > 0
                && size.rows > 0
                && size.width >= size.columns
                && size.height >= size.rows =>
        {
            (
                u32::from(size.width / size.columns),
                u32::from(size.height / size.rows),
            )
        }
        _ => (8, 16),
    }
}

fn handle_config_key(
    app: &mut App,
    picker: &mut SettingsPicker,
    key: KeyEvent,
) -> InlineResult<(BrowseDefaults, DisplaySettings)> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return InlineResult::Cancel;
    }

    match key.code {
        KeyCode::Up | KeyCode::Char('k') => picker.move_up(),
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => picker.move_down(),
        KeyCode::Left | KeyCode::Char('h') => picker.change(app, false),
        KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => picker.change(app, true),
        KeyCode::Enter => {
            return InlineResult::Accept((app.browse_defaults(), app.display_settings().clone()));
        }
        KeyCode::Escape | KeyCode::Char('q') => return InlineResult::Cancel,
        _ => {}
    }
    InlineResult::Continue
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
        KeyCode::Escape | KeyCode::Char('q') => return InlineResult::Cancel,
        _ => {}
    }
    InlineResult::Continue
}

fn desired_viewport_height(
    app: &App,
    terminal_width: u16,
    terminal_height: u16,
    min_height: u16,
) -> u16 {
    if app.is_viewing() {
        return terminal_height.saturating_sub(1).clamp(1, 30);
    }
    let status_row = usize::from(app.status().is_some());
    let current_rows = app.entries().len().saturating_add(status_row);
    let body_rows = if terminal_width >= TWO_PANE_MIN_WIDTH {
        let preview_rows = if let Some(document) = app.preview().document() {
            document
                .lines
                .len()
                .saturating_add(usize::from(document.notice.is_some()))
        } else if app.preview().image().is_some() {
            8
        } else if app.preview().message().is_some() {
            3
        } else {
            app.preview().entries().len()
        };
        current_rows.max(preview_rows).saturating_add(1)
    } else {
        current_rows
    };
    let content_height = body_rows.saturating_add(2);
    let content_height = u16::try_from(content_height)
        .unwrap_or(u16::MAX)
        .clamp(3, 14)
        .max(min_height);
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

    if app.is_viewing() {
        let scroll = app.viewer_scroll();
        let step = if app.is_rendered_markdown_view() {
            app.markdown_scroll_step()
        } else {
            1
        };
        let page = page_rows.saturating_mul(step);
        match key.code {
            KeyCode::Escape | KeyCode::Char('q') => app.close_viewer(),
            KeyCode::Up | KeyCode::Char('k') => app.set_viewer_scroll(scroll.saturating_sub(step)),
            KeyCode::Down | KeyCode::Char('j') => {
                app.set_viewer_scroll(scroll.saturating_add(step))
            }
            KeyCode::PageUp => app.set_viewer_scroll(scroll.saturating_sub(page)),
            KeyCode::PageDown => app.set_viewer_scroll(scroll.saturating_add(page)),
            KeyCode::Home => app.set_viewer_scroll(0),
            KeyCode::End => app.set_viewer_scroll(usize::MAX),
            KeyCode::Char('o') => return app.open_selected(),
            KeyCode::Char('m') => app.toggle_markdown_source(),
            KeyCode::Char('R') => app.toggle_reading(),
            _ => {}
        }
        return NavigationResult::Continue;
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
            KeyCode::Escape => app.clear_filter(),
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
        KeyCode::Char('w') => save_current_defaults(app),
        KeyCode::Char('v') => app.open_viewer(),
        KeyCode::Char('o') => return app.open_selected(),
        KeyCode::Enter => return app.accept(),
        KeyCode::Escape | KeyCode::Char('q') => return NavigationResult::Cancel,
        _ => {}
    }
    NavigationResult::Continue
}

fn save_current_defaults(app: &mut App) {
    match save_defaults(app.browse_defaults()) {
        Ok(path) => app.set_status(format!("saved defaults to {}", path.display())),
        Err(error) => app.set_status(format!("cannot save defaults: {error}")),
    }
}

/// Build the editor invocation from `$VISUAL`, then `$EDITOR`. On Unix the
/// value runs through `sh`, like Git does, so editors with arguments such as
/// `code --wait` work.
fn editor_command(path: &Path) -> Result<Command, String> {
    let editor = ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(env::var_os)
        .find(|value| !value.is_empty());

    #[cfg(unix)]
    {
        let mut script = editor.unwrap_or_else(|| OsString::from("vi"));
        script.push(" \"$@\"");
        let mut command = Command::new("sh");
        command.arg("-c").arg(script).arg("sh").arg(path);
        Ok(command)
    }

    #[cfg(not(unix))]
    {
        let editor = editor.ok_or("set VISUAL or EDITOR to choose an editor")?;
        let editor = editor
            .to_str()
            .ok_or("VISUAL or EDITOR must be valid UTF-8")?;
        let mut parts = editor.split_whitespace();
        let program = parts
            .next()
            .ok_or("set VISUAL or EDITOR to choose an editor")?;
        let mut command = Command::new(program);
        command.args(parts).arg(path);
        Ok(command)
    }
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
    mouse_capture: bool,
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.mouse_capture {
            let _ = execute!(io::stderr(), DisableMouseCapture);
        }
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
    fn mouse_capture_follows_valid_origin_and_restores_terminal_modes() {
        let mut active = false;
        let mut output = Vec::new();
        set_mouse_capture(&mut active, false, &mut output).unwrap();
        assert!(output.is_empty());
        set_mouse_capture(&mut active, true, &mut output).unwrap();
        assert!(active);
        let enabled_len = output.len();
        set_mouse_capture(&mut active, true, &mut output).unwrap();
        assert_eq!(output.len(), enabled_len);
        set_mouse_capture(&mut active, false, &mut output).unwrap();
        assert!(!active);
        assert!(output.len() > enabled_len);
    }

    #[test]
    fn invalid_origin_allows_resize_retry_but_timeout_does_not() {
        let mut supported = true;
        assert_eq!(resolve_origin(Some(None), &mut supported), None);
        assert!(supported);
        assert_eq!(resolve_origin(Some(Some(3)), &mut supported), Some(3));
        assert!(supported);
        assert_eq!(resolve_origin(None, &mut supported), None);
        assert!(!supported);
    }

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
    fn parses_config_and_its_subcommands() {
        let cli = Cli::try_parse_from(["de", "config", "path"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Config {
                command: Some(ConfigCommand::Path)
            })
        ));

        let cli = Cli::try_parse_from(["de", "config", "edit"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Config {
                command: Some(ConfigCommand::Edit)
            })
        ));

        let cli = Cli::try_parse_from(["de", "config"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Config { command: None })
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

        let escape = KeyEvent::new(KeyCode::Escape, KeyModifiers::NONE);
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
    fn mouse_scroll_hits_and_drag_clamps_at_measured_origin() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("notes.txt"), "line\n".repeat(100)).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let area = Rect::new(0, 0, 40, 10);
        let mut drag = None;
        let event = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let down = MouseEventKind::ScrollDown;
        assert!(!handle_viewer_mouse(
            &mut app,
            area,
            Some(14),
            event(down, 2, 16),
            &mut drag
        ));
        app.open_viewer();
        let raw_max = viewer_scroll_max(&app, area);
        app.toggle_reading();
        assert!(viewer_scroll_max(&app, area) < raw_max);
        assert!(!handle_viewer_mouse(
            &mut app,
            area,
            None,
            event(down, 2, 16),
            &mut drag,
        ));
        assert!(!handle_viewer_mouse(
            &mut app,
            area,
            Some(14),
            event(down, 2, 13),
            &mut drag
        ));
        assert!(!handle_viewer_mouse(
            &mut app,
            area,
            Some(14),
            event(down, 2, 14),
            &mut drag
        ));
        assert!(handle_viewer_mouse(
            &mut app,
            area,
            Some(14),
            event(down, 2, 16),
            &mut drag
        ));
        assert_eq!(app.viewer_scroll(), 3);
        let bar = viewer_scrollbar(&app, area).unwrap();
        let x = bar.track.x;
        handle_viewer_mouse(
            &mut app,
            area,
            Some(14),
            event(MouseEventKind::Down(MouseButton::Left), x, 14 + bar.thumb.y),
            &mut drag,
        );
        assert!(drag.is_some());
        handle_viewer_mouse(
            &mut app,
            area,
            Some(14),
            event(MouseEventKind::Drag(MouseButton::Left), 0, 23),
            &mut drag,
        );
        assert_eq!(app.viewer_scroll(), bar.maximum);
        handle_viewer_mouse(
            &mut app,
            area,
            Some(14),
            event(MouseEventKind::Drag(MouseButton::Left), 0, 0),
            &mut drag,
        );
        assert_eq!(app.viewer_scroll(), 0);
        handle_viewer_mouse(
            &mut app,
            area,
            Some(14),
            event(MouseEventKind::Up(MouseButton::Left), 0, 0),
            &mut drag,
        );
        assert!(drag.is_none());
        handle_viewer_mouse(
            &mut app,
            area,
            Some(14),
            event(
                MouseEventKind::Down(MouseButton::Left),
                x,
                14 + bar.track.bottom() - 1,
            ),
            &mut drag,
        );
        assert_eq!(app.viewer_scroll(), bar.maximum);
        app.close_viewer();
        handle_viewer_mouse(&mut app, area, Some(14), event(down, 2, 16), &mut drag);
        assert!(drag.is_none());
    }

    #[test]
    fn viewer_keys_keep_selection_and_scroll_without_navigating() {
        let temp = tempdir().unwrap();
        fs::write(
            temp.path().join("notes.txt"),
            "one\ntwo\nthree\nfour\nfive\n",
        )
        .unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        handle_key(&mut app, key(KeyCode::Char('v')), 2);
        assert!(app.is_viewing());
        handle_key(&mut app, key(KeyCode::PageDown), 2);
        assert_eq!(app.viewer_scroll(), 2);
        handle_key(&mut app, key(KeyCode::Char('k')), 2);
        assert_eq!(app.viewer_scroll(), 1);
        handle_key(&mut app, key(KeyCode::End), 2);
        assert_eq!(app.viewer_scroll(), usize::MAX);
        handle_key(&mut app, key(KeyCode::Home), 2);
        assert_eq!(app.viewer_scroll(), 0);
        handle_key(&mut app, key(KeyCode::Char('q')), 2);
        assert!(!app.is_viewing());
        assert_eq!(app.current_dir(), temp.path());
        assert_eq!(app.selected_entry().unwrap().name, "notes.txt");
    }

    #[test]
    fn v_is_filter_text_during_filter_input() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("view.txt"), "text").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_filter();
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
            3,
        );
        assert_eq!(app.filter_query(), "v");
        assert!(!app.is_viewing());
    }

    #[test]
    fn markdown_mode_is_viewer_only_and_resets_scroll() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("manual.md"), "# Title\n\nfirst\nsecond\n").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let key = |character| KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
        handle_key(&mut app, key('m'), 2);
        assert!(!app.is_markdown_source());
        handle_key(&mut app, key('v'), 2);
        app.set_viewer_scroll(5);
        handle_key(&mut app, key('m'), 2);
        assert!(app.is_markdown_source());
        assert_eq!(app.viewer_scroll(), 0);
        handle_key(&mut app, key('m'), 2);
        assert!(!app.is_markdown_source());
        handle_key(&mut app, key('q'), 2);
        app.begin_filter();
        handle_key(&mut app, key('m'), 2);
        assert_eq!(app.filter_query(), "m");
        assert!(!app.is_viewing());
    }

    #[test]
    fn reading_key_resets_on_mode_and_view_changes_and_rejects_code() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("a.txt"), "first\nsecond\nthird").unwrap();
        fs::write(temp.path().join("b.rs"), "fn main() {}\n").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let key = |code| KeyEvent::new(KeyCode::Char(code), KeyModifiers::NONE);
        handle_key(&mut app, key('v'), 2);
        app.set_viewer_scroll(3);
        handle_key(&mut app, key('R'), 2);
        assert!(app.is_reading());
        assert_eq!(app.viewer_scroll(), 0);
        app.set_viewer_scroll(2);
        handle_key(&mut app, key('R'), 2);
        assert!(!app.is_reading());
        assert_eq!(app.viewer_scroll(), 0);
        handle_key(&mut app, key('R'), 2);
        handle_key(&mut app, key('q'), 2);
        handle_key(&mut app, key('v'), 2);
        assert!(!app.is_reading());
        handle_key(&mut app, key('q'), 2);
        app.move_down();
        handle_key(&mut app, key('v'), 2);
        handle_key(&mut app, key('R'), 2);
        assert!(!app.is_reading());
        assert!(app.status().unwrap().contains("plain text only"));
    }

    #[test]
    fn markdown_navigation_uses_rendered_pixels_and_source_rows() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("manual.md"), "# Title\n\nText").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        handle_key(&mut app, key(KeyCode::Char('v')), 3);
        handle_key(&mut app, key(KeyCode::Down), 3);
        assert_eq!(app.viewer_scroll(), 16);
        handle_key(&mut app, key(KeyCode::PageDown), 3);
        assert_eq!(app.viewer_scroll(), 64);
        handle_key(&mut app, key(KeyCode::PageUp), 3);
        assert_eq!(app.viewer_scroll(), 16);
        handle_key(&mut app, key(KeyCode::Home), 3);
        assert_eq!(app.viewer_scroll(), 0);
        handle_key(&mut app, key(KeyCode::End), 3);
        assert_eq!(app.viewer_scroll(), usize::MAX);
        handle_key(&mut app, key(KeyCode::Char('m')), 3);
        handle_key(&mut app, key(KeyCode::Down), 3);
        assert_eq!(app.viewer_scroll(), 1);
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
