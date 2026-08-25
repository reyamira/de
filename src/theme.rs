use crate::DisplaySettings;
use ratatui::style::Color;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, value};

pub const THEME_ENV: &str = "DE_THEME";

const BUILTIN_NAMES: [&str; 8] = [
    "auto", "light", "dark", "mono", "ocean", "forest", "amber", "rose",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Theme {
    name: String,
    palette: Palette,
}

impl Theme {
    fn new(name: impl Into<String>, palette: Palette) -> Self {
        Self {
            name: name.into(),
            palette,
        }
    }

    pub fn auto() -> Self {
        Self::new("auto", auto_palette())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn palette(&self) -> Palette {
        self.palette
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    themes: Vec<Theme>,
    saved_theme: Option<String>,
    display: DisplaySettings,
}

impl Config {
    pub fn built_ins() -> Self {
        Self {
            themes: built_in_themes(),
            saved_theme: None,
            display: DisplaySettings::default(),
        }
    }

    pub fn load() -> io::Result<Self> {
        let path = theme_config_path()?;
        match fs::read_to_string(&path) {
            Ok(contents) => Self::from_toml(&contents),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut catalog = Self::built_ins();
                catalog.saved_theme = load_legacy_theme(&path)?;
                Ok(catalog)
            }
            Err(error) => Err(error),
        }
    }

    fn from_toml(contents: &str) -> io::Result<Self> {
        let document = parse_document(contents)?;
        let mut catalog = Self::built_ins();
        catalog.display = DisplaySettings::from_document(&document)?;

        if let Some(item) = document.get("theme") {
            if let Some(name) = item.as_str() {
                catalog.saved_theme = Some(name.to_owned());
            } else if let Some(theme) = item.as_table() {
                if let Some(selected) = theme.get("selected") {
                    let name = selected
                        .as_str()
                        .ok_or_else(|| invalid_data("theme.selected must be a string"))?;
                    catalog.saved_theme = Some(name.to_owned());
                }
            } else {
                return Err(invalid_data(
                    "theme must be a table; the legacy top-level value must be a string",
                ));
            }
        }

        let Some(themes_item) = document.get("themes") else {
            return Ok(catalog);
        };
        let themes = themes_item
            .as_table()
            .ok_or_else(|| invalid_data("themes must be a table"))?;

        for (name, item) in themes {
            validate_theme_name(name).map_err(|error| {
                invalid_data(format!("invalid custom theme name {name:?}: {error}"))
            })?;
            if catalog.find(name).is_some() {
                return Err(invalid_data(format!(
                    "custom theme {name:?} conflicts with another theme"
                )));
            }
            let table = item
                .as_table()
                .ok_or_else(|| invalid_data(format!("custom theme {name:?} must be a table")))?;
            let theme = parse_custom_theme(name, table, &catalog.themes)?;
            catalog.themes.push(theme);
        }

        Ok(catalog)
    }

    pub fn find(&self, name: &str) -> Option<&Theme> {
        let name = name.trim();
        self.themes
            .iter()
            .find(|theme| theme.name.eq_ignore_ascii_case(name))
    }

    pub fn saved_theme(&self) -> Option<&str> {
        self.saved_theme.as_deref()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.themes.iter().map(|theme| theme.name.as_str())
    }

    pub const fn display(&self) -> &DisplaySettings {
        &self.display
    }

    pub fn next(&self, current: &str) -> Theme {
        let index = self
            .themes
            .iter()
            .position(|theme| theme.name.eq_ignore_ascii_case(current))
            .unwrap_or(0);
        self.themes[(index + 1) % self.themes.len()].clone()
    }

    pub fn previous(&self, current: &str) -> Theme {
        let index = self
            .themes
            .iter()
            .position(|theme| theme.name.eq_ignore_ascii_case(current))
            .unwrap_or(0);
        self.themes[(index + self.themes.len() - 1) % self.themes.len()].clone()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Palette {
    pub accent: Color,
    pub text: Color,
    pub muted: Color,
    pub title: Color,
    pub error: Color,
    pub symlink: Color,
    pub emphasis_foreground: Color,
    pub emphasis_background: Color,
    pub reverse_emphasis: bool,
    pub dim_muted: bool,
}

pub fn theme_config_path() -> io::Result<PathBuf> {
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME") {
        let config_home = PathBuf::from(config_home);
        if config_home.is_absolute() {
            return Ok(config_home.join("de/config.toml"));
        }
    }

    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config/de/config.toml"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "cannot find a config directory; set XDG_CONFIG_HOME or HOME",
            )
        })
}

pub fn save_theme(name: &str) -> io::Result<PathBuf> {
    validate_theme_name(name)?;
    let path = theme_config_path()?;
    save_theme_at(&path, name)?;
    Ok(path)
}

pub fn create_custom_theme(name: &str) -> io::Result<PathBuf> {
    validate_theme_name(name)?;
    if is_builtin_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{name:?} is a built-in theme"),
        ));
    }

    let path = theme_config_path()?;
    create_custom_theme_at(&path, name)?;
    Ok(path)
}

fn save_theme_at(path: &Path, name: &str) -> io::Result<()> {
    let mut document = read_document_or_new(path)?;
    let legacy_prefix = document
        .as_table()
        .key("theme")
        .and_then(|key| key.leaf_decor().prefix().cloned());
    let legacy_suffix = document
        .get("theme")
        .and_then(Item::as_value)
        .and_then(|value| value.decor().suffix().cloned());
    let mut migrated_legacy_value = false;
    match document.get_mut("theme") {
        Some(item) if item.is_table() => {
            item.as_table_mut()
                .expect("the item was checked as a table above")["selected"] = value(name);
        }
        Some(item) if item.as_str().is_some() => {
            let mut theme = Table::new();
            theme["selected"] = value(name);
            *item = Item::Table(theme);
            migrated_legacy_value = true;
        }
        Some(_) => {
            return Err(invalid_data(
                "theme must be a table; the legacy top-level value must be a string",
            ));
        }
        None => {
            let mut theme = Table::new();
            theme["selected"] = value(name);
            document["theme"] = Item::Table(theme);
        }
    }
    if migrated_legacy_value {
        document
            .as_table_mut()
            .key_mut("theme")
            .expect("the migrated theme table has a key")
            .leaf_decor_mut()
            .clear();
        let decor = document["theme"]
            .as_table_mut()
            .expect("the legacy value was replaced with a table")
            .decor_mut();
        if let Some(prefix) = legacy_prefix {
            decor.set_prefix(prefix);
        }
        if let Some(suffix) = legacy_suffix {
            decor.set_suffix(suffix);
        }
    }
    write_document(path, &document)
}

fn create_custom_theme_at(path: &Path, name: &str) -> io::Result<()> {
    let mut document = read_document_or_new(path)?;
    if !document.contains_key("themes") {
        let mut themes = Table::new();
        themes.set_implicit(true);
        document["themes"] = Item::Table(themes);
    }
    let themes = document["themes"]
        .as_table_mut()
        .ok_or_else(|| invalid_data("themes must be a table"))?;
    if themes
        .iter()
        .any(|(existing, _)| existing.eq_ignore_ascii_case(name))
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("custom theme {name:?} already exists"),
        ));
    }

    let mut custom = Table::new();
    custom["extends"] = value("dark");
    custom["accent"] = value("#7dd3fc");
    custom["text"] = value("default");
    custom["muted"] = value("#94a3b8");
    custom["title"] = value("#c4b5fd");
    custom["error"] = value("#fb7185");
    custom["symlink"] = value("#f0abfc");
    custom["selection_fg"] = value("#0f172a");
    custom["selection_bg"] = value("#67e8f9");
    custom["reverse_selection"] = value(false);
    custom["dim_muted"] = value(false);
    themes[name] = Item::Table(custom);

    write_document(path, &document)
}

fn read_document_or_new(path: &Path) -> io::Result<DocumentMut> {
    match fs::read_to_string(path) {
        Ok(contents) => parse_document(&contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(error) => Err(error),
    }
}

fn parse_document(contents: &str) -> io::Result<DocumentMut> {
    contents
        .parse::<DocumentMut>()
        .map_err(|error| invalid_data(format!("invalid config.toml: {error}")))
}

fn write_document(path: &Path, document: &DocumentMut) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config path has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::write(path, document.to_string())
}

fn load_legacy_theme(config_path: &Path) -> io::Result<Option<String>> {
    let legacy_path = config_path.with_file_name("theme");
    let value = match fs::read_to_string(legacy_path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let value = value.trim();
    if !is_builtin_name(value) {
        return Err(invalid_data(format!(
            "unknown legacy saved theme {value:?}"
        )));
    }
    Ok(Some(value.to_ascii_lowercase()))
}

fn parse_custom_theme(name: &str, table: &Table, built_ins: &[Theme]) -> io::Result<Theme> {
    let extends = optional_string(table, "extends", name)?.unwrap_or("auto");
    let base = built_ins
        .iter()
        .find(|theme| theme.name.eq_ignore_ascii_case(extends) && is_builtin_name(&theme.name))
        .ok_or_else(|| {
            invalid_data(format!(
                "custom theme {name:?} extends unknown built-in theme {extends:?}"
            ))
        })?;
    let mut palette = base.palette;

    apply_color(table, "accent", name, &mut palette.accent)?;
    apply_color(table, "text", name, &mut palette.text)?;
    apply_color(table, "muted", name, &mut palette.muted)?;
    apply_color(table, "title", name, &mut palette.title)?;
    apply_color(table, "error", name, &mut palette.error)?;
    apply_color(table, "symlink", name, &mut palette.symlink)?;
    apply_color(
        table,
        "selection_fg",
        name,
        &mut palette.emphasis_foreground,
    )?;
    apply_color(
        table,
        "selection_bg",
        name,
        &mut palette.emphasis_background,
    )?;
    apply_bool(
        table,
        "reverse_selection",
        name,
        &mut palette.reverse_emphasis,
    )?;
    apply_bool(table, "dim_muted", name, &mut palette.dim_muted)?;

    Ok(Theme::new(name, palette))
}

fn optional_string<'a>(table: &'a Table, key: &str, theme: &str) -> io::Result<Option<&'a str>> {
    match table.get(key) {
        None => Ok(None),
        Some(item) => item.as_str().map(Some).ok_or_else(|| {
            invalid_data(format!(
                "{key:?} in custom theme {theme:?} must be a string"
            ))
        }),
    }
}

fn apply_color(table: &Table, key: &str, theme: &str, target: &mut Color) -> io::Result<()> {
    let Some(value) = optional_string(table, key, theme)? else {
        return Ok(());
    };
    *target = parse_color(value).ok_or_else(|| {
        invalid_data(format!(
            "invalid color {value:?} for {key:?} in custom theme {theme:?}"
        ))
    })?;
    Ok(())
}

fn apply_bool(table: &Table, key: &str, theme: &str, target: &mut bool) -> io::Result<()> {
    let Some(item) = table.get(key) else {
        return Ok(());
    };
    *target = item.as_bool().ok_or_else(|| {
        invalid_data(format!(
            "{key:?} in custom theme {theme:?} must be true or false"
        ))
    })?;
    Ok(())
}

fn parse_color(value: &str) -> Option<Color> {
    let normalized = value.trim().to_ascii_lowercase();
    if let Some(hex) = normalized.strip_prefix('#') {
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let red = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let green = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let blue = u8::from_str_radix(&hex[4..6], 16).ok()?;
        return Some(Color::Rgb(red, green, blue));
    }

    Some(match normalized.as_str() {
        "default" | "reset" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "dark-gray" | "dark-grey" => Color::DarkGray,
        "light-red" => Color::LightRed,
        "light-green" => Color::LightGreen,
        "light-yellow" => Color::LightYellow,
        "light-blue" => Color::LightBlue,
        "light-magenta" => Color::LightMagenta,
        "light-cyan" => Color::LightCyan,
        "white" => Color::White,
        _ => return None,
    })
}

fn validate_theme_name(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name.len() > 32
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "theme names must be 1-32 ASCII letters, numbers, hyphens, or underscores",
        ));
    }
    Ok(())
}

fn is_builtin_name(name: &str) -> bool {
    BUILTIN_NAMES
        .iter()
        .any(|built_in| built_in.eq_ignore_ascii_case(name))
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn built_in_themes() -> Vec<Theme> {
    vec![
        Theme::new("auto", auto_palette()),
        Theme::new(
            "light",
            Palette {
                accent: Color::Blue,
                text: Color::Black,
                muted: Color::DarkGray,
                title: Color::Blue,
                error: Color::Red,
                symlink: Color::Magenta,
                emphasis_foreground: Color::White,
                emphasis_background: Color::Blue,
                reverse_emphasis: false,
                dim_muted: false,
            },
        ),
        Theme::new(
            "dark",
            Palette {
                accent: Color::LightCyan,
                text: Color::White,
                muted: Color::Gray,
                title: Color::LightBlue,
                error: Color::LightRed,
                symlink: Color::LightMagenta,
                emphasis_foreground: Color::Black,
                emphasis_background: Color::LightCyan,
                reverse_emphasis: false,
                dim_muted: false,
            },
        ),
        Theme::new(
            "mono",
            Palette {
                accent: Color::Reset,
                text: Color::Reset,
                muted: Color::Reset,
                title: Color::Reset,
                error: Color::Reset,
                symlink: Color::Reset,
                emphasis_foreground: Color::Reset,
                emphasis_background: Color::Reset,
                reverse_emphasis: true,
                dim_muted: true,
            },
        ),
        Theme::new(
            "ocean",
            vivid_palette(
                (56, 189, 248),
                (148, 163, 184),
                (129, 140, 248),
                (251, 113, 133),
                (192, 132, 252),
                (8, 47, 73),
                (125, 211, 252),
            ),
        ),
        Theme::new(
            "forest",
            vivid_palette(
                (74, 222, 128),
                (134, 167, 137),
                (163, 230, 53),
                (251, 113, 133),
                (250, 204, 21),
                (5, 46, 22),
                (134, 239, 172),
            ),
        ),
        Theme::new(
            "amber",
            vivid_palette(
                (251, 191, 36),
                (168, 162, 158),
                (251, 146, 60),
                (248, 113, 113),
                (244, 114, 182),
                (69, 26, 3),
                (252, 211, 77),
            ),
        ),
        Theme::new(
            "rose",
            vivid_palette(
                (251, 113, 133),
                (161, 161, 170),
                (244, 114, 182),
                (248, 113, 113),
                (192, 132, 252),
                (76, 5, 25),
                (253, 164, 175),
            ),
        ),
    ]
}

const fn auto_palette() -> Palette {
    Palette {
        accent: Color::Cyan,
        text: Color::Reset,
        muted: Color::Gray,
        title: Color::Cyan,
        error: Color::Red,
        symlink: Color::Magenta,
        emphasis_foreground: Color::Reset,
        emphasis_background: Color::Reset,
        reverse_emphasis: true,
        dim_muted: false,
    }
}

const fn rgb(color: (u8, u8, u8)) -> Color {
    Color::Rgb(color.0, color.1, color.2)
}

const fn vivid_palette(
    accent: (u8, u8, u8),
    muted: (u8, u8, u8),
    title: (u8, u8, u8),
    error: (u8, u8, u8),
    symlink: (u8, u8, u8),
    selection_foreground: (u8, u8, u8),
    selection_background: (u8, u8, u8),
) -> Palette {
    Palette {
        accent: rgb(accent),
        text: Color::Reset,
        muted: rgb(muted),
        title: rgb(title),
        error: rgb(error),
        symlink: rgb(symlink),
        emphasis_foreground: rgb(selection_foreground),
        emphasis_background: rgb(selection_background),
        reverse_emphasis: false,
        dim_muted: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn built_in_themes_cycle_in_both_directions() {
        let catalog = Config::built_ins();
        assert_eq!(catalog.next("auto").name(), "light");
        assert_eq!(catalog.previous("auto").name(), "rose");
        assert_eq!(catalog.next("rose").name(), "auto");
    }

    #[test]
    fn config_loads_a_saved_custom_theme_with_overrides() {
        let catalog = Config::from_toml(
            r##"
[theme]
selected = "midnight"

[display]
date_format = "relative"

[themes.midnight]
extends = "dark"
accent = "#123456"
selection_bg = "light-cyan"
dim_muted = true
"##,
        )
        .unwrap();

        assert_eq!(catalog.saved_theme(), Some("midnight"));
        assert_eq!(
            catalog.display().date_format(),
            &crate::DateFormat::Relative
        );
        let midnight = catalog.find("midnight").unwrap();
        assert_eq!(midnight.palette().accent, Color::Rgb(0x12, 0x34, 0x56));
        assert_eq!(midnight.palette().text, Color::White);
        assert_eq!(midnight.palette().emphasis_background, Color::LightCyan);
        assert!(midnight.palette().dim_muted);
    }

    #[test]
    fn legacy_top_level_theme_is_still_read() {
        let config = Config::from_toml("theme = \"dark\"\n").unwrap();
        assert_eq!(config.saved_theme(), Some("dark"));
    }

    #[test]
    fn grouped_theme_selection_updates_without_losing_other_settings() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("de/config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "[theme]\nselected = \"auto\"\nfuture_setting = \"keep\"\n",
        )
        .unwrap();

        save_theme_at(&path, "forest").unwrap();
        let saved = fs::read_to_string(path).unwrap();
        assert!(saved.contains("selected = \"forest\""));
        assert!(saved.contains("future_setting = \"keep\""));
    }

    #[test]
    fn saving_only_changes_theme_and_preserves_comments_and_tables() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("de/config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "# keep this comment\ntheme = \"auto\" # keep inline\n\n[themes.night]\nextends = \"dark\"\n",
        )
        .unwrap();

        save_theme_at(&path, "night").unwrap();
        let saved = fs::read_to_string(path).unwrap();
        assert!(saved.contains("# keep this comment"));
        assert!(saved.contains("# keep inline"));
        assert!(saved.contains("[theme]"), "saved config:\n{saved}");
        assert!(saved.contains("selected = \"night\""));
        assert!(!saved.contains("theme = \"auto\""));
        assert!(saved.contains("[themes.night]"));
    }

    #[test]
    fn scaffold_is_non_destructive_and_loadable() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("de/config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "# preferences\n[theme]\nselected = \"forest\"\n").unwrap();

        create_custom_theme_at(&path, "midnight").unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("# preferences"));
        assert!(contents.contains("selected = \"forest\""));
        assert!(contents.contains("[themes.midnight]"));
        assert!(
            Config::from_toml(&contents)
                .unwrap()
                .find("midnight")
                .is_some()
        );
        assert_eq!(
            create_custom_theme_at(&path, "midnight")
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn invalid_custom_values_are_rejected() {
        let bad_color = Config::from_toml(
            r##"[themes.neon]
extends = "dark"
accent = "#123"
"##,
        )
        .unwrap_err();
        assert_eq!(bad_color.kind(), io::ErrorKind::InvalidData);

        let bad_parent =
            Config::from_toml("[themes.neon]\nextends = \"another-custom-theme\"\n").unwrap_err();
        assert_eq!(bad_parent.kind(), io::ErrorKind::InvalidData);

        let non_ascii_hex =
            Config::from_toml("[themes.neon]\nextends = \"dark\"\naccent = \"#aéaaa\"\n")
                .unwrap_err();
        assert_eq!(non_ascii_hex.kind(), io::ErrorKind::InvalidData);

        let duplicate = Config::from_toml(
            "[themes.night]\nextends = \"dark\"\n[themes.NIGHT]\nextends = \"light\"\n",
        )
        .unwrap_err();
        assert_eq!(duplicate.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn auto_and_mono_use_adaptive_reverse_emphasis() {
        let catalog = Config::built_ins();
        assert!(catalog.find("auto").unwrap().palette().reverse_emphasis);
        assert!(catalog.find("mono").unwrap().palette().reverse_emphasis);
        assert!(!catalog.find("dark").unwrap().palette().reverse_emphasis);
    }
}
