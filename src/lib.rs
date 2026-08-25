mod app;
pub mod backend;
mod theme;
mod ui;

pub use app::{App, Entry, NavigationResult, Preview, SortDirection, SortMode};
pub use theme::{
    Palette, THEME_ENV, Theme, ThemeCatalog, create_custom_theme, save_theme, theme_config_path,
};
pub use ui::{TWO_PANE_MIN_WIDTH, render, render_theme_preview};

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
        -h|--help|-V|--version|init|theme)
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
            case -h --help -V --version init theme
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
    fn shell_init_bypasses_the_wrapper_function() {
        let init = shell_init("bash").unwrap();
        assert!(init.contains("command de"));
        assert!(init.contains("builtin cd --"));
        assert!(init.contains("-h|--help|-V|--version|init|theme"));
    }

    #[test]
    fn rejects_unknown_shells() {
        assert_eq!(shell_init("powershell"), None);
    }
}
