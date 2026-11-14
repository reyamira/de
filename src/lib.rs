mod app;
pub mod backend;
mod config;
pub mod document;
pub mod editor;
mod fmd_assets;
mod fmd_diagram;
#[path = "../vendor/franken-markdown/src/parse/mod.rs"]
mod fmd_parse;
#[path = "../vendor/franken-markdown/src/svg.rs"]
mod fmd_svg;
mod image_loader;
mod markdown_renderer;
pub(crate) mod matching;
pub mod media;
pub mod render_theme;
mod rendered_text;
pub mod search;
mod settings;
mod svg;
#[path = "../vendor/franken-markdown/src/table_format.rs"]
mod table_format;
mod theme;
pub mod theme_editor;
pub mod theme_editor_ui;
mod ui;

pub use app::{App, Entry, NavigationResult, Preview, SortDirection, SortMode, ThemeSample};
pub use config::{BrowseDefaults, DateFormat, DisplaySettings, TimeFormat, Timezone};
pub use settings::{Setting, SettingsPicker};
pub use theme::{
    Config, Palette, THEME_ENV, Theme, create_custom_theme, create_custom_theme_from,
    ensure_config_file, load_custom_theme, new_custom_theme, preview_custom_theme,
    save_custom_theme, save_defaults, save_settings, save_theme, theme_config_path,
};
pub use ui::{
    CONFIG_PICKER_MIN_HEIGHT, TWO_PANE_MIN_WIDTH, ViewerScrollBar, browser_sample_page_rows,
    browser_sample_scroll_max, editor_body, editor_page_rows, editor_scrollbar, preview_image_area,
    render, render_config_picker, render_theme_preview, resume_editor_cursor, set_editor_scroll,
    viewer_body, viewer_page_rows, viewer_scroll_max, viewer_scrollbar,
};

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::{env, fs, io};

/// Return the shell's logical working directory when it still refers to the
/// process' actual working directory. This preserves paths reached through a
/// symlink instead of unexpectedly replacing `$PWD` with a physical path.
pub fn logical_current_dir() -> io::Result<PathBuf> {
    let actual = env::current_dir()?;

    let Some(pwd) = env::var_os("PWD") else {
        return Ok(actual);
    };
    let candidate = PathBuf::from(pwd);
    if !candidate.is_absolute() {
        return Ok(actual);
    }

    match (fs::canonicalize(&candidate), fs::canonicalize(&actual)) {
        (Ok(candidate), Ok(actual)) if candidate == actual => Ok(PathBuf::from(
            env::var_os("PWD").expect("PWD was present above"),
        )),
        _ => Ok(actual),
    }
}

pub fn resolve_start_path(path: Option<&OsStr>) -> io::Result<PathBuf> {
    match path {
        None => logical_current_dir(),
        Some(path) => {
            let path = Path::new(path);
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                logical_current_dir()?.join(path)
            };
            fs::canonicalize(absolute)
        }
    }
}

pub fn shell_init(shell: &str) -> Option<&'static str> {
    match shell {
        "bash" | "zsh" => Some(
            r#"de() {
    case "${1-}" in
        -h|--help|-V|--version|init|theme|config|languages)
            command de "$@"
            return
            ;;
    esac

    local de_dir
    de_dir="$(command de "$@")" || return
    [[ -n "$de_dir" ]] && builtin cd -- "$de_dir"
}"#,
        ),
        "fish" => Some(
            r#"function de
    if test (count $argv) -gt 0
        switch $argv[1]
            case -h --help -V --version init theme config languages
                command de $argv
                return
        end
    end

    set --local de_dir (command de $argv)
    or return
    test -n "$de_dir"; and cd -- "$de_dir"
end"#,
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_shells() {
        assert_eq!(shell_init("powershell"), None);
    }
}
