use clap::builder::styling::{AnsiColor, Styles};
use clap::{Parser, Subcommand, ValueEnum};
use crossterm::cursor;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{self, Clear, ClearType, disable_raw_mode, enable_raw_mode};
use de::backend::InlineBackend;
use de::theme_editor::ThemeEditorDraft;
use de::theme_editor_ui::{EditorAction, ThemeEditorUi};
use de::{
    App, BrowseDefaults, CONFIG_PICKER_MIN_HEIGHT, Config, DisplaySettings, NavigationResult,
    SettingsPicker, THEME_ENV, TWO_PANE_MIN_WIDTH, Theme, ThemeSample, create_custom_theme,
    ensure_config_file, preview_image_area, render, render_config_picker, render_theme_preview,
    resolve_start_path, save_defaults, save_settings, save_theme, shell_init, theme_config_path,
    viewer_body, viewer_page_rows, viewer_scroll_max, viewer_scrollbar,
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
  /                  Recursive names + text search
  Shift+S            Ascending / descending
  v                  View file inline   Esc / q           Return to picker
  f (viewer)         Toggle inline / full terminal size
  e                  Edit original file inline (pane or viewer)
  Ctrl-S             Save in editor; Esc/Ctrl-Q returns with Save/Discard/Cancel
  Shift+arrows        Select; Ctrl-Z / Ctrl-Y undo / redo in editor
  m                  Toggle Markdown source / rendered inside the viewer
  / (viewer)         Find in file; Enter accepts; Escape restores the view
  n / N (viewer)     Next / previous match
  y (viewer)         Send selected text to terminal clipboard (OSC52)
  Mouse              Wheel or scrollbar scrolls; drag viewer text to select
  R                  Toggle plain-text reading / raw inside the viewer
  o                  Open file with its default application
  . / r              Hidden / refresh
  w                  Save hidden and sort settings as defaults
  Enter              Choose selected folder / view file
  Ctrl-G             Choose current header folder, including empty folders

Run `de theme` to preview and save a color theme, or `de theme edit` to edit one.
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

    /// Preview, save, create, or edit themes
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
    /// Edit a custom theme or create a copy of a built-in theme
    Edit {
        /// Theme to edit (defaults to the selected theme)
        name: Option<String>,
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
        Some(CliCommand::Theme {
            command: Some(ThemeCommand::Edit { name }),
        }) => {
            require_terminal()?;
            let catalog = Config::load().map_err(|error| format!("cannot load config: {error}"))?;
            let selected = resolve_theme(name.or(theme), &catalog)?;
            let builtin = Config::built_ins().find(selected.name()).is_some();
            let table = if builtin {
                de::new_custom_theme(selected.name())
            } else {
                de::load_custom_theme(selected.name())
            }
            .map_err(|error| format!("cannot edit theme: {error}"))?;
            let draft_name = if builtin { "draft" } else { selected.name() };
            let draft = ThemeEditorDraft::new(draft_name.to_owned(), table)
                .map_err(|error| format!("cannot edit theme: {error}"))?;
            let start = resolve_start_path(None)
                .map_err(|error| format!("cannot resolve current directory: {error}"))?;
            let mut app = App::with_defaults(start, catalog.defaults())
                .map_err(|error| format!("cannot open current directory: {error}"))?;
            app.set_display_settings(catalog.display().clone());
            app.set_theme(draft.preview().clone());
            if let Some((name, path)) =
                run_theme_editor(app, draft, builtin).map_err(|error| error.to_string())?
            {
                eprintln!("Saved theme {name} to {}", path.display());
            }
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
    run_inline_with_mouse(
        app,
        (),
        InlineOptions {
            min_height: 11,
            images: true,
            editor_mouse: false,
        },
        |frame, app, ()| render_theme_preview(frame, app),
        |app, (), key, page_rows, area| {
            let rows = if app.theme_sample() == ThemeSample::Picker {
                de::browser_sample_page_rows(area)
            } else {
                page_rows
            };
            handle_theme_key(app, key, catalog, rows.max(1))
        },
        |_, _, _, _| InlineResult::Continue,
        |_, area| Some(area),
    )
}

fn run_theme_editor(
    app: App,
    draft: ThemeEditorDraft,
    builtin: bool,
) -> io::Result<Option<(String, std::path::PathBuf)>> {
    run_inline_with_mouse(
        app,
        ThemeEditorUi::new(draft, builtin),
        InlineOptions {
            min_height: 18,
            images: true,
            editor_mouse: true,
        },
        |frame, app, editor| editor.draw(frame, app),
        |app, editor, key, _, area| {
            let action = editor.handle_key(app, key, area);
            handle_editor_action(editor, action, builtin)
        },
        |app, editor, mouse, area| {
            let action = editor.handle_mouse(app, mouse, area);
            handle_editor_action(editor, action, builtin)
        },
        |editor, area| editor.preview_area(area),
    )
}

fn handle_editor_action(
    editor: &mut ThemeEditorUi,
    action: EditorAction,
    builtin: bool,
) -> InlineResult<(String, std::path::PathBuf)> {
    match action {
        EditorAction::Continue => InlineResult::Continue,
        EditorAction::Cancel => InlineResult::Cancel,
        EditorAction::Save(name) => {
            let saved = if builtin {
                de::create_custom_theme_from(&name, editor.draft.table())
            } else {
                de::save_custom_theme(&name, editor.draft.table())
            };
            match saved {
                Ok(path) => InlineResult::Accept((name, path)),
                Err(error) => {
                    editor.set_error(error.to_string());
                    InlineResult::Continue
                }
            }
        }
    }
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
    app: App,
    state: S,
    min_height: u16,
    images: bool,
    draw: impl FnMut(&mut ratatui::Frame<'_>, &App, &S),
    mut handle: impl FnMut(&mut App, &mut S, KeyEvent, usize) -> InlineResult<T>,
) -> io::Result<Option<T>> {
    run_inline_with_mouse(
        app,
        state,
        InlineOptions {
            min_height,
            images,
            editor_mouse: false,
        },
        draw,
        |app, state, key, rows, _| handle(app, state, key, rows),
        |_, _, _, _| InlineResult::Continue,
        |_, area| Some(area),
    )
}

struct InlineOptions {
    min_height: u16,
    images: bool,
    editor_mouse: bool,
}

fn run_inline_with_mouse<S, T>(
    mut app: App,
    mut state: S,
    options: InlineOptions,
    mut draw: impl FnMut(&mut ratatui::Frame<'_>, &App, &S),
    mut handle: impl FnMut(&mut App, &mut S, KeyEvent, usize, Rect) -> InlineResult<T>,
    mut handle_mouse: impl FnMut(&mut App, &mut S, MouseEvent, Rect) -> InlineResult<T>,
    preview_area: impl Fn(&S, Rect) -> Option<Rect>,
) -> io::Result<Option<T>> {
    let InlineOptions {
        min_height,
        images,
        editor_mouse,
    } = options;
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
        let mut bracketed_paste = false;
        let mut drag = None;
        let mut origin = None;
        let mut origin_supported = true;
        loop {
            redraw |= app.poll_preview();
            redraw |= app.poll_markdown();
            redraw |= app.poll_search();
            if redraw {
                let paste_wanted = app.editor().is_some()
                    || app.find_prompt()
                    || (app.is_searching() && !app.is_viewing());
                if paste_wanted != bracketed_paste {
                    if paste_wanted {
                        execute!(io::stderr(), EnableBracketedPaste)?;
                    } else {
                        execute!(io::stderr(), DisableBracketedPaste)?;
                    }
                    bracketed_paste = paste_wanted;
                }
                let viewing = app.is_viewing() || app.editor().is_some() || editor_mouse;
                if viewer_active && !viewing {
                    drag = None;
                    set_mouse_capture(&mut raw_mode.mouse_capture, false, &mut io::stderr())?;
                    viewer_active = false;
                }
                let desired =
                    desired_viewport_height(&app, terminal_width, terminal_height, min_height);
                let mut measure_origin = viewing && !viewer_active;
                if desired != viewport_height {
                    set_mouse_capture(&mut raw_mode.mouse_capture, false, &mut io::stderr())?;
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
                let preview = preview_area(&state, area);
                if app.editor().is_none() {
                    app.reveal_viewer_source(area);
                }
                if app.is_viewing()
                    && app.editor().is_none()
                    && let Some(preview) = preview
                {
                    app.set_viewer_scroll(
                        app.viewer_scroll().min(viewer_scroll_max(&app, preview)),
                    );
                }
                if let Some(body) = preview
                    .filter(|_| app.is_rendered_markdown_view())
                    .map(viewer_body)
                {
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
            let image = if images
                && app.editor().is_none()
                && (!editor_mouse || app.is_rendered_markdown_view())
            {
                preview_area(&state, area).and_then(|preview| {
                    app.markdown_image()
                        .or_else(|| {
                            (!app.is_rendered_markdown_view())
                                .then(|| app.preview().image())
                                .flatten()
                        })
                        .zip(preview_image_area(&app, preview))
                })
            } else {
                None
            };
            terminal.backend_mut().draw_image(image)?;

            // Read input between transfer batches, and wake for prepared images.
            if terminal.backend().image_pending() {
                if !reader.poll(Some(Duration::ZERO), |_| true)? {
                    continue;
                }
            } else if app.background_work_pending()
                && !reader.poll(Some(Duration::from_millis(16)), |_| true)?
            {
                continue;
            }
            match reader.read(|_| true)? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    let page_rows = if app.editor().is_some() {
                        de::editor_page_rows(&app, area)
                    } else if app.is_viewing() {
                        viewer_page_rows(area)
                    } else {
                        visible_entry_rows(&app, terminal_width, viewport_height)
                    };
                    match handle(&mut app, &mut state, key, page_rows, area) {
                        InlineResult::Continue => {
                            drag = None;
                            redraw = true;
                        }
                        InlineResult::Accept(value) => break Ok(Some(value)),
                        InlineResult::Cancel => break Ok(None),
                    }
                }
                Event::Paste(text) => {
                    de::resume_editor_cursor(&app);
                    app.paste(&text);
                    redraw = true;
                }
                Event::Mouse(mouse) if viewer_active => {
                    if editor_mouse {
                        if let Some(origin) = origin
                            && mouse.row >= origin
                            && mouse.row - origin < area.height
                            && mouse.column < area.width
                        {
                            let mut relative = mouse;
                            relative.row -= origin;
                            match handle_mouse(&mut app, &mut state, relative, area) {
                                InlineResult::Continue => redraw = true,
                                InlineResult::Accept(value) => break Ok(Some(value)),
                                InlineResult::Cancel => break Ok(None),
                            }
                        }
                    } else {
                        redraw |= handle_viewer_mouse(&mut app, area, origin, mouse, &mut drag);
                    }
                }
                Event::WindowResized(size) => {
                    let (width, height) = (size.cols, size.rows);
                    set_mouse_capture(&mut raw_mode.mouse_capture, false, &mut io::stderr())?;
                    drag = None;
                    app.resize_viewer_interaction();
                    terminal_width = width;
                    terminal_height = height;
                    viewport_height = desired_viewport_height(&app, width, height, min_height);
                    terminal
                        .backend_mut()
                        .resize_viewport(width, viewport_height)?;
                    terminal.resize(Rect::new(0, 0, width, viewport_height))?;
                    origin = None;
                    if (app.is_viewing() || app.editor().is_some() || editor_mouse)
                        && origin_supported
                    {
                        terminal.backend_mut().request_origin()?;
                        let result = read_origin(&reader, width, height, viewport_height)?;
                        origin = resolve_origin(result, &mut origin_supported);
                    }
                    set_mouse_capture(
                        &mut raw_mode.mouse_capture,
                        (app.is_viewing() || app.editor().is_some() || editor_mouse)
                            && origin.is_some(),
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

#[derive(Clone, Copy, Debug)]
enum MouseDrag {
    Scroll(u16),
    Selection,
}

fn handle_viewer_mouse(
    app: &mut App,
    area: Rect,
    origin: Option<u16>,
    mouse: MouseEvent,
    drag: &mut Option<MouseDrag>,
) -> bool {
    if !app.is_viewing() && app.editor().is_none() {
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
    let editing = app.editor().is_some();
    let container = if editing {
        de::editor_body(app, area)
    } else if area.height < 3 {
        area
    } else {
        Rect::new(area.x, area.y + 1, area.width, area.height - 2)
    };
    let point = ratatui::layout::Position::new(mouse.column, row);
    let bar = if editing {
        de::editor_scrollbar(app, area)
    } else {
        viewer_scrollbar(app, area)
    };
    let current = bar.map_or(app.viewer_scroll(), |bar| bar.offset);
    let set_scroll = |app: &mut App, scroll| {
        if editing {
            de::set_editor_scroll(app, scroll, area);
        } else {
            app.set_viewer_scroll(scroll);
        }
    };
    match mouse.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            if inside && container.contains(point) =>
        {
            let step = if !editing && app.is_rendered_markdown_view() {
                app.markdown_scroll_step()
            } else {
                1
            }
            .saturating_mul(3);
            let maximum = bar.map_or_else(|| viewer_scroll_max(app, area), |bar| bar.maximum);
            let next = if mouse.kind == MouseEventKind::ScrollUp {
                current.saturating_sub(step)
            } else {
                current.saturating_add(step)
            }
            .min(maximum);
            set_scroll(app, next);
            next != current
        }
        MouseEventKind::Down(MouseButton::Left) if inside => {
            if let Some(bar) = bar
                && bar.track.contains(point)
            {
                if bar.thumb.contains(point) {
                    *drag = Some(MouseDrag::Scroll(row - bar.thumb.y));
                    return false;
                }
                let grab = bar.thumb.height / 2;
                *drag = Some(MouseDrag::Scroll(grab));
                let next = bar.scroll_at(row, grab);
                let changed = next != current;
                set_scroll(app, next);
                return changed;
            }
            if !editing && viewer_body(area).contains(point) {
                let (width, height) = terminal_cell_pixels();
                if let Some(byte) = app.viewer_hit_test(area, mouse.column, row, width, height) {
                    app.begin_viewer_selection(byte);
                    *drag = Some(MouseDrag::Selection);
                    return true;
                }
            }
            false
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(MouseDrag::Scroll(grab)) = *drag
                && let Some(bar) = bar
            {
                let next = bar.scroll_at(row, grab);
                let changed = next != current;
                set_scroll(app, next);
                return changed;
            }
            if matches!(drag, Some(MouseDrag::Selection)) && !editing {
                let (width, height) = terminal_cell_pixels();
                if let Some(byte) = app.viewer_hit_test(area, mouse.column, row, width, height) {
                    app.extend_viewer_selection(byte);
                    return true;
                }
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

fn handle_theme_key(
    app: &mut App,
    key: KeyEvent,
    catalog: &Config,
    page_rows: usize,
) -> InlineResult<Theme> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return InlineResult::Cancel;
    }

    match key.code {
        KeyCode::Left | KeyCode::Up | KeyCode::Char('h') | KeyCode::Char('k') => {
            app.set_theme(catalog.previous(app.theme().name()));
        }
        KeyCode::Right | KeyCode::Down | KeyCode::Char('l') | KeyCode::Char('j') => {
            app.set_theme(catalog.next(app.theme().name()))
        }
        KeyCode::Tab => {
            let next = match app.theme_sample() {
                ThemeSample::Picker => ThemeSample::Source,
                ThemeSample::Source => ThemeSample::Rendered,
                ThemeSample::Rendered => ThemeSample::Picker,
            };
            app.set_theme_sample(next);
        }
        KeyCode::PageUp | KeyCode::PageDown if app.theme_sample() == ThemeSample::Picker => {
            let maximum = de::browser_sample_scroll_max(page_rows);
            let current = app.viewer_scroll().min(maximum);
            let next = if key.code == KeyCode::PageUp {
                current.saturating_sub(page_rows)
            } else {
                current.saturating_add(page_rows).min(maximum)
            };
            app.set_viewer_scroll(next);
        }
        KeyCode::Char('m') if app.is_viewing() => app.toggle_markdown_source(),
        KeyCode::PageUp if app.is_viewing() => {
            let step = if app.is_rendered_markdown_view() {
                app.markdown_scroll_step()
            } else {
                1
            };
            app.set_viewer_scroll(
                app.viewer_scroll()
                    .saturating_sub(page_rows.saturating_mul(step)),
            );
        }
        KeyCode::PageDown if app.is_viewing() => {
            let step = if app.is_rendered_markdown_view() {
                app.markdown_scroll_step()
            } else {
                1
            };
            app.set_viewer_scroll(
                app.viewer_scroll()
                    .saturating_add(page_rows.saturating_mul(step)),
            );
        }
        KeyCode::Home if app.is_viewing() => app.set_viewer_scroll(0),
        KeyCode::End if app.is_viewing() => app.set_viewer_scroll(usize::MAX),
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
    if app.viewer_expanded() {
        // The relative backend reserves these rows without an alternate screen.
        return terminal_height.max(1);
    }
    if app.is_viewing() || app.editor().is_some() {
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
    if app.editor().is_some() {
        app.handle_editor_key(key, page_rows);
        return NavigationResult::Continue;
    }
    if app.is_viewing()
        && ((!app.find_prompt() && key.code == KeyCode::Char('y'))
            || (key.modifiers.contains(KeyModifiers::CONTROL)
                && key.code == KeyCode::Char('c')
                && app.has_viewer_selection()))
    {
        if let Err(error) = app.copy_viewer_selection(&mut io::stderr()) {
            app.set_status(format!("Cannot send clipboard: {error}"));
        }
        return NavigationResult::Continue;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return NavigationResult::Cancel;
    }
    if app.is_viewing() && app.find_prompt() {
        match key.code {
            KeyCode::Escape => app.finish_find(true),
            KeyCode::Enter => app.finish_find(false),
            KeyCode::Backspace => app.pop_find_char(),
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                app.push_find_char(ch)
            }
            _ => {}
        }
        return NavigationResult::Continue;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('g') {
        return app.accept_current();
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
            KeyCode::Char('f') => app.toggle_viewer_expanded(),
            KeyCode::Char('e') => app.open_editor(),
            KeyCode::Char('/') => app.begin_find(),
            KeyCode::Char('n') => app.next_match(false),
            KeyCode::Char('N') => app.next_match(true),
            _ => {}
        }
        return NavigationResult::Continue;
    }

    if app.is_searching() {
        match key.code {
            KeyCode::Up => app.move_up(),
            KeyCode::Down => app.move_down(),
            KeyCode::PageUp => app.page_up(page_rows),
            KeyCode::PageDown => app.page_down(page_rows),
            KeyCode::Home => app.move_first(),
            KeyCode::End => app.move_last(),
            KeyCode::Right | KeyCode::Tab => app.enter_selected(),
            KeyCode::Left => app.go_parent(),
            KeyCode::Backspace => app.pop_search_char(),
            KeyCode::Enter => return app.activate_selected(),
            KeyCode::Escape => app.cancel_search(),
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                app.push_search_char(character);
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
        KeyCode::Char('/') => app.begin_search(),
        KeyCode::Char('s') => app.cycle_sort(),
        KeyCode::Char('S') => app.toggle_sort_direction(),
        KeyCode::Char('.') => app.toggle_hidden(),
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Char('w') => save_current_defaults(app),
        KeyCode::Char('v') => app.open_viewer(),
        KeyCode::Char('e') => app.open_editor(),
        KeyCode::Char('o') => return app.open_selected(),
        KeyCode::Enter => return app.activate_selected(),
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
        let _ = execute!(io::stderr(), DisableBracketedPaste);
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
    fn viewer_expansion_follows_terminal_size_and_resets_after_closing() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("notes.txt"), "line\n".repeat(180)).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let normal_picker = desired_viewport_height(&app, 112, 48, 0);
        handle_key(&mut app, key(KeyCode::Char('f')), 26);
        assert!(!app.viewer_expanded());
        assert_eq!(desired_viewport_height(&app, 112, 48, 0), normal_picker);
        app.open_viewer();
        app.set_viewer_scroll(50);
        assert_eq!(desired_viewport_height(&app, 112, 48, 0), 30);
        handle_key(&mut app, key(KeyCode::Char('f')), 26);
        assert!(app.viewer_expanded());
        assert_eq!(app.viewer_scroll(), 50);
        for (width, height) in [(112, 48), (80, 60), (40, 18), (1, 1)] {
            assert_eq!(desired_viewport_height(&app, width, height, 0), height);
        }
        let area = Rect::new(0, 0, 112, 48);
        assert_eq!(viewer_body(area), Rect::new(1, 2, 109, 44));
        app.set_viewer_scroll(usize::MAX);
        app.set_viewer_scroll(app.viewer_scroll().min(viewer_scroll_max(&app, area)));
        assert_eq!(app.viewer_scroll(), viewer_scroll_max(&app, area));
        let scroll = app.viewer_scroll();
        handle_key(&mut app, key(KeyCode::Char('f')), 44);
        assert!(!app.viewer_expanded());
        assert_eq!(desired_viewport_height(&app, 112, 48, 0), 30);
        assert_eq!(app.viewer_scroll(), scroll);
        handle_key(&mut app, key(KeyCode::Char('f')), 26);
        handle_key(&mut app, key(KeyCode::Char('e')), 44);
        assert!(app.editor().is_some());
        assert_eq!(desired_viewport_height(&app, 112, 48, 0), 48);
        handle_key(&mut app, key(KeyCode::Char('f')), 44);
        assert!(app.viewer_expanded());
        assert!(app.editor().unwrap().dirty());
        handle_key(&mut app, key(KeyCode::Escape), 44);
        handle_key(&mut app, key(KeyCode::Char('d')), 44);
        assert!(app.editor().is_none());
        assert!(app.viewer_expanded());
        handle_key(&mut app, key(KeyCode::Escape), 44);
        assert!(!app.is_viewing());
        assert!(!app.viewer_expanded());
        assert_eq!(desired_viewport_height(&app, 112, 48, 0), normal_picker);
        app.open_viewer();
        assert!(!app.viewer_expanded());
        assert_eq!(desired_viewport_height(&app, 112, 48, 0), 30);
    }

    #[test]
    fn viewer_expansion_key_remains_text_in_find_prompt() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("notes.txt"), "first\n").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_viewer();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        handle_key(&mut app, key(KeyCode::Char('/')), 26);
        handle_key(&mut app, key(KeyCode::Char('f')), 26);
        assert!(app.find_status().unwrap().contains("f"));
        assert!(!app.viewer_expanded());
        handle_key(&mut app, key(KeyCode::Escape), 26);
        handle_key(&mut app, key(KeyCode::Char('f')), 26);
        assert!(app.viewer_expanded());
        handle_key(&mut app, key(KeyCode::Char('/')), 44);
        handle_key(&mut app, key(KeyCode::Char('f')), 44);
        assert!(app.viewer_expanded());
    }

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

        let cli = Cli::try_parse_from(["de", "theme", "edit", "midnight"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Theme {
                command: Some(ThemeCommand::Edit { name: Some(name) })
            }) if name == "midnight"
        ));
        let cli = Cli::try_parse_from(["de", "theme", "edit"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Theme {
                command: Some(ThemeCommand::Edit { name: None })
            })
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
    fn slash_starts_search_and_escape_restores_before_canceling() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("source")).unwrap();
        fs::write(temp.path().join("notes.txt"), "notes").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();

        let slash = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
        assert_eq!(handle_key(&mut app, slash, 5), NavigationResult::Continue);
        assert!(app.is_searching());

        let character = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
        assert_eq!(
            handle_key(&mut app, character, 5),
            NavigationResult::Continue
        );
        assert_eq!(app.search_query(), "s");
        assert!(app.entries().is_empty());

        let escape = KeyEvent::new(KeyCode::Escape, KeyModifiers::NONE);
        assert_eq!(handle_key(&mut app, escape, 5), NavigationResult::Continue);
        assert!(!app.is_searching());
        assert!(app.search_query().is_empty());
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
    fn enter_views_file_and_control_g_accepts_current_folder() {
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
        assert_eq!(handle_key(&mut app, enter, 5), NavigationResult::Continue);
        assert!(app.is_viewing());
        assert_eq!(
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
                5
            ),
            NavigationResult::Accept(temp.path().to_path_buf())
        );
    }

    #[test]
    fn o_remains_query_text_during_search() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("notes.txt"), "notes").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_search();

        let open = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(handle_key(&mut app, open, 5), NavigationResult::Continue);
        assert_eq!(app.search_query(), "o");
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
    fn viewer_drag_copies_original_tabs_clusters_and_newlines_without_gutters() {
        let temp = tempdir().unwrap();
        let source = "first\tCafe\u{301}\r\nsecond needle end\r\n";
        fs::write(temp.path().join("notes.txt"), source).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_viewer();
        let area = Rect::new(0, 0, 50, 10);
        let body = viewer_body(area);
        let origin = 12;
        let event = |kind, x, y| MouseEvent {
            kind,
            column: x,
            row: origin + y,
            modifiers: KeyModifiers::NONE,
        };
        let mut drag = None;
        assert!(handle_viewer_mouse(
            &mut app,
            area,
            Some(origin),
            event(
                MouseEventKind::Down(MouseButton::Left),
                body.x + 3 + 5,
                body.y
            ),
            &mut drag
        ));
        assert!(handle_viewer_mouse(
            &mut app,
            area,
            Some(origin),
            event(
                MouseEventKind::Drag(MouseButton::Left),
                body.x + 3 + 13,
                body.y + 1
            ),
            &mut drag
        ));
        assert_eq!(
            app.selected_viewer_text(),
            Some("\tCafe\u{301}\r\nsecond needle")
        );
        handle_viewer_mouse(
            &mut app,
            area,
            Some(origin),
            event(MouseEventKind::Up(MouseButton::Left), 0, 0),
            &mut drag,
        );
        assert!(drag.is_none());
        assert_eq!(
            fs::read_to_string(temp.path().join("notes.txt")).unwrap(),
            source
        );
    }

    #[test]
    fn editor_wheel_and_scrollbar_drag_keep_text_history_and_caret() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("notes.txt"), "line\n".repeat(100)).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_editor();
        let area = Rect::new(0, 0, 80, 12);
        let body = de::editor_body(&app, area);
        let origin = 14;
        let event = |kind, x, y| MouseEvent {
            kind,
            column: x,
            row: origin + y,
            modifiers: KeyModifiers::NONE,
        };
        let mut drag = None;
        let caret = app.editor().unwrap().cursor();
        let revision = app.editor().unwrap().revision();
        assert!(handle_viewer_mouse(
            &mut app,
            area,
            Some(origin),
            event(MouseEventKind::ScrollDown, body.x + 1, body.y + 1),
            &mut drag
        ));
        let bar = de::editor_scrollbar(&app, area).unwrap();
        assert_eq!(bar.offset, 3);
        handle_viewer_mouse(
            &mut app,
            area,
            Some(origin),
            event(
                MouseEventKind::Down(MouseButton::Left),
                bar.track.x,
                bar.thumb.y,
            ),
            &mut drag,
        );
        handle_viewer_mouse(
            &mut app,
            area,
            Some(origin),
            event(
                MouseEventKind::Drag(MouseButton::Left),
                bar.track.x,
                area.height + 20,
            ),
            &mut drag,
        );
        assert_eq!(
            de::editor_scrollbar(&app, area).unwrap().offset,
            bar.maximum
        );
        assert_eq!(app.editor().unwrap().cursor(), caret);
        assert_eq!(app.editor().unwrap().revision(), revision);
        assert!(!app.editor().unwrap().dirty());
    }

    #[test]
    fn viewer_find_prompt_keeps_letters_local_and_escape_restores_scroll() {
        let temp = tempdir().unwrap();
        fs::write(
            temp.path().join("notes.txt"),
            "zero\nneedle\nother NEEDLE\n",
        )
        .unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_viewer();
        app.set_viewer_scroll(1);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        handle_key(&mut app, key(KeyCode::Char('/')), 4);
        for ch in "needle".chars() {
            handle_key(&mut app, key(KeyCode::Char(ch)), 4);
        }
        assert!(app.editor().is_none());
        assert!(app.find_status().unwrap().contains("1 / 2"));
        assert_eq!(
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                4
            ),
            NavigationResult::Cancel
        );
        assert!(app.find_prompt());
        handle_key(&mut app, key(KeyCode::Escape), 4);
        assert!(app.is_viewing());
        assert_eq!(app.viewer_scroll(), 1);
        assert!(!app.find_prompt());
        handle_key(&mut app, key(KeyCode::Escape), 4);
        assert!(!app.is_viewing());
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
    fn v_remains_query_text_during_search() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("view.txt"), "text").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_search();
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
            3,
        );
        assert_eq!(app.search_query(), "v");
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
        app.begin_search();
        handle_key(&mut app, key('m'), 2);
        assert_eq!(app.search_query(), "m");
        assert!(!app.is_viewing());
    }

    #[test]
    fn editor_routes_control_c_and_plain_picker_letters_to_buffer() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("notes.txt");
        fs::write(&path, "original").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        handle_key(&mut app, key(KeyCode::Char('e')), 5);
        assert!(app.editor().is_some());
        assert_eq!(
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                5
            ),
            NavigationResult::Continue
        );
        handle_key(&mut app, key(KeyCode::Char('q')), 5);
        assert!(app.editor().unwrap().lines()[0].starts_with('q'));
        app.paste("λ\r\npasted");
        assert_eq!(app.editor().unwrap().lines().len(), 2);
        assert_eq!(fs::read_to_string(path).unwrap(), "original");
    }

    #[test]
    fn enter_selects_child_and_empty_enter_never_accepts() {
        let temp = tempdir().unwrap();
        let child = temp.path().join("empty");
        fs::create_dir(&child).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            handle_key(&mut app, enter, 3),
            NavigationResult::Accept(child.clone())
        );
        app.enter_selected();
        assert_eq!(handle_key(&mut app, enter, 3), NavigationResult::Continue);
        assert_eq!(
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
                3
            ),
            NavigationResult::Accept(child)
        );
        app.begin_search();
        app.push_search_char('z');
        assert_eq!(handle_key(&mut app, enter, 3), NavigationResult::Continue);
        assert_eq!(
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Escape, KeyModifiers::NONE),
                3
            ),
            NavigationResult::Continue
        );
        assert!(!app.is_searching());
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
            handle_theme_key(&mut app, right, &catalog, 4),
            InlineResult::Continue
        ));
        assert_eq!(app.theme().name(), "light");

        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        assert!(matches!(
            handle_theme_key(&mut app, left, &catalog, 4),
            InlineResult::Continue
        ));
        assert_eq!(app.theme().name(), "auto");

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        match handle_theme_key(&mut app, enter, &catalog, 4) {
            InlineResult::Accept(theme) => assert_eq!(theme.name(), "auto"),
            _ => panic!("Enter should accept the previewed theme"),
        }
    }

    #[test]
    fn theme_browser_sample_scrolls_without_changing_theme() {
        let temp = tempdir().unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let catalog = Config::built_ins();
        let down = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);
        let up = KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE);
        handle_theme_key(&mut app, down, &catalog, 3);
        assert!(app.viewer_scroll() > 0);
        assert_eq!(app.theme_sample(), ThemeSample::Picker);
        assert_eq!(app.theme().name(), "auto");
        handle_theme_key(&mut app, down, &catalog, 3);
        handle_theme_key(&mut app, down, &catalog, 3);
        assert_eq!(app.viewer_scroll(), de::browser_sample_scroll_max(3));
        handle_theme_key(&mut app, up, &catalog, 3);
        assert!(app.viewer_scroll() < de::browser_sample_scroll_max(3));
    }

    #[test]
    fn theme_sample_keys_cycle_and_keep_theme_selection() {
        let temp = tempdir().unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let catalog = Config::built_ins();
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        handle_theme_key(&mut app, tab, &catalog, 4);
        assert_eq!(app.theme_sample(), ThemeSample::Source);
        handle_theme_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
            &catalog,
            4,
        );
        assert_eq!(app.theme_sample(), ThemeSample::Rendered);
        handle_theme_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &catalog,
            4,
        );
        assert_eq!(app.theme().name(), "light");
        assert_eq!(app.theme_sample(), ThemeSample::Rendered);
        handle_theme_key(&mut app, tab, &catalog, 4);
        assert_eq!(app.theme_sample(), ThemeSample::Picker);
        assert!(!app.is_viewing());
        assert!(matches!(
            handle_theme_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                &catalog,
                4
            ),
            InlineResult::Cancel
        ));
    }
}
