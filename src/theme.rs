use crate::render_theme::RenderedTheme;
use crate::{BrowseDefaults, DisplaySettings};
use ratatui::style::Color;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use toml_edit::{DocumentMut, Item, Table, value};

pub const THEME_ENV: &str = "DE_THEME";

const BUILTIN_NAMES: [&str; 8] = [
    "auto", "light", "dark", "mono", "ocean", "forest", "amber", "rose",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Theme {
    name: String,
    palette: Palette,
    rendered: Arc<RenderedTheme>,
}

impl Theme {
    fn new(name: impl Into<String>, mut palette: Palette) -> Self {
        let name = name.into();
        palette.syntax = SyntaxPalette::from_interface(&palette);
        palette.markdown = MarkdownPalette::from_interface(&palette);
        let rendered = RenderedTheme::defaults(!name.eq_ignore_ascii_case("light"))
            .resolve(Path::new("."))
            .expect("bundled rendered theme must be valid");
        Self {
            name,
            palette,
            rendered,
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

    pub fn rendered(&self) -> Arc<RenderedTheme> {
        Arc::clone(&self.rendered)
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    themes: Vec<Theme>,
    saved_theme: Option<String>,
    display: DisplaySettings,
    defaults: BrowseDefaults,
}

impl Config {
    pub fn built_ins() -> Self {
        Self {
            themes: built_in_themes(),
            saved_theme: None,
            display: DisplaySettings::default(),
            defaults: BrowseDefaults::default(),
        }
    }

    pub fn load() -> io::Result<Self> {
        let path = theme_config_path()?;
        match fs::read_to_string(&path) {
            Ok(contents) => Self::from_toml_at(&contents, path.parent().unwrap_or(Path::new("."))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut catalog = Self::built_ins();
                catalog.saved_theme = load_legacy_theme(&path)?;
                Ok(catalog)
            }
            Err(error) => Err(error),
        }
    }

    #[cfg(test)]
    fn from_toml(contents: &str) -> io::Result<Self> {
        Self::from_toml_at(contents, Path::new("."))
    }

    fn from_toml_at(contents: &str, config_dir: &Path) -> io::Result<Self> {
        let document = parse_document(contents)?;
        let mut catalog = Self::built_ins();
        catalog.display = DisplaySettings::from_document(&document)?;
        catalog.defaults = BrowseDefaults::from_document(&document)?;

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
            let theme = parse_custom_theme(name, table, &catalog.themes, config_dir)?;
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

    pub const fn defaults(&self) -> BrowseDefaults {
        self.defaults
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
    pub syntax: SyntaxPalette,
    pub markdown: MarkdownPalette,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxPalette {
    pub text: Color,
    pub comment: Color,
    pub keyword: Color,
    pub string: Color,
    pub number: Color,
    pub constant: Color,
    pub entity: Color,
    pub support: Color,
}

impl SyntaxPalette {
    const fn from_interface(p: &Palette) -> Self {
        Self {
            text: p.text,
            comment: p.muted,
            keyword: p.title,
            string: p.accent,
            number: p.symlink,
            constant: p.symlink,
            entity: p.title,
            support: p.symlink,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownPalette {
    pub text: Color,
    pub heading: Color,
    pub link: Color,
    pub link_url: Color,
    pub code: Color,
    pub punctuation: Color,
    pub marker: Color,
}

impl MarkdownPalette {
    const fn from_interface(p: &Palette) -> Self {
        Self {
            text: p.text,
            heading: p.title,
            link: p.symlink,
            link_url: p.muted,
            code: p.accent,
            punctuation: p.muted,
            marker: p.accent,
        }
    }
}

const EMPTY_SYNTAX: SyntaxPalette = SyntaxPalette {
    text: Color::Reset,
    comment: Color::Reset,
    keyword: Color::Reset,
    string: Color::Reset,
    number: Color::Reset,
    constant: Color::Reset,
    entity: Color::Reset,
    support: Color::Reset,
};
const EMPTY_MARKDOWN: MarkdownPalette = MarkdownPalette {
    text: Color::Reset,
    heading: Color::Reset,
    link: Color::Reset,
    link_url: Color::Reset,
    code: Color::Reset,
    punctuation: Color::Reset,
    marker: Color::Reset,
};

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

/// Create config.toml if it does not exist yet, so it can be opened in an
/// editor. An existing file is left untouched.
pub fn ensure_config_file() -> io::Result<PathBuf> {
    let path = theme_config_path()?;
    ensure_config_file_at(&path)?;
    Ok(path)
}

fn ensure_config_file_at(path: &Path) -> io::Result<()> {
    match fs::metadata(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            write_document(path, &read_document_or_new(path)?)
        }
        Err(error) => Err(error),
    }
}

/// Save the picker's current toggles as the starting state for future runs.
pub fn save_defaults(defaults: BrowseDefaults) -> io::Result<PathBuf> {
    let path = theme_config_path()?;
    save_defaults_at(&path, defaults)?;
    Ok(path)
}

/// Save everything the `de config` picker edits in one write.
pub fn save_settings(defaults: BrowseDefaults, display: &DisplaySettings) -> io::Result<PathBuf> {
    let path = theme_config_path()?;
    save_settings_at(&path, defaults, display)?;
    Ok(path)
}

fn save_settings_at(
    path: &Path,
    defaults: BrowseDefaults,
    display: &DisplaySettings,
) -> io::Result<()> {
    let mut document = read_document_or_new(path)?;
    defaults.write_to(&mut document)?;
    display.write_to(&mut document)?;
    write_document(path, &document)
}

fn save_defaults_at(path: &Path, defaults: BrowseDefaults) -> io::Result<()> {
    let mut document = read_document_or_new(path)?;
    defaults.write_to(&mut document)?;
    write_document(path, &document)
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

/// Start a sparse custom theme that inherits an existing built-in theme.
pub fn new_custom_theme(base: &str) -> io::Result<Table> {
    if !is_builtin_name(base) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown built-in theme {base:?}"),
        ));
    }
    let mut draft = Table::new();
    draft["extends"] = value(base);
    Ok(draft)
}

/// Load the editable, unexpanded table so omitted values remain inherited.
pub fn load_custom_theme(name: &str) -> io::Result<Table> {
    let path = theme_config_path()?;
    load_custom_theme_at(&path, name)
}

fn load_custom_theme_at(path: &Path, name: &str) -> io::Result<Table> {
    validate_theme_name(name)?;
    let document = read_document_or_new(path)?;
    let themes = document.get("themes").and_then(Item::as_table);
    let item = themes.and_then(|themes| {
        themes
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
    });
    let (_, item) = item.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("custom theme {name:?} not found"),
        )
    })?;
    item.as_table()
        .cloned()
        .ok_or_else(|| invalid_data(format!("custom theme {name:?} must be a table")))
}

/// Resolve a draft with the same validation and relative paths as config loading.
pub fn preview_custom_theme(name: &str, draft: &Table) -> io::Result<Theme> {
    let path = theme_config_path()?;
    preview_custom_theme_at(&path, name, draft)
}

pub(crate) fn preview_custom_theme_at(path: &Path, name: &str, draft: &Table) -> io::Result<Theme> {
    validate_theme_name(name)?;
    if is_builtin_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{name:?} is a built-in theme"),
        ));
    }
    parse_custom_theme(
        name,
        draft,
        &built_in_themes(),
        path.parent().unwrap_or(Path::new(".")),
    )
}

/// Save an existing custom theme without changing the rest of the config.
pub fn save_custom_theme(name: &str, draft: &Table) -> io::Result<PathBuf> {
    let path = theme_config_path()?;
    save_custom_theme_at(&path, name, draft, false)?;
    Ok(path)
}

/// Create a custom theme without overwriting an existing name.
pub fn create_custom_theme_from(name: &str, draft: &Table) -> io::Result<PathBuf> {
    let path = theme_config_path()?;
    save_custom_theme_at(&path, name, draft, true)?;
    Ok(path)
}

fn save_custom_theme_at(path: &Path, name: &str, draft: &Table, create: bool) -> io::Result<()> {
    preview_custom_theme_at(path, name, draft)?;
    let mut document = read_document_or_new(path)?;
    if !document.contains_key("themes") {
        if !create {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("custom theme {name:?} not found"),
            ));
        }
        let mut themes = Table::new();
        themes.set_implicit(true);
        document["themes"] = Item::Table(themes);
    }
    let themes = document["themes"]
        .as_table_mut()
        .ok_or_else(|| invalid_data("themes must be a table"))?;
    let existing = themes
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(key, _)| key.to_owned());
    match (create, existing) {
        (true, Some(_)) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("custom theme {name:?} already exists"),
            ));
        }
        (false, None) => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("custom theme {name:?} not found"),
            ));
        }
        (_, key) => themes[key.as_deref().unwrap_or(name)] = Item::Table(draft.clone()),
    }
    Config::from_toml_at(
        &document.to_string(),
        path.parent().unwrap_or(Path::new(".")),
    )?;
    write_document(path, &document)
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
    let mut syntax = Table::new();
    syntax
        .decor_mut()
        .set_prefix("\n# Source colors are independent of interface colors.\n");
    for (key, color) in [
        ("text", "white"),
        ("comment", "gray"),
        ("keyword", "light-blue"),
        ("string", "light-cyan"),
        ("number", "light-magenta"),
        ("constant", "light-magenta"),
        ("entity", "light-blue"),
        ("support", "light-magenta"),
    ] {
        syntax[key] = value(color);
    }
    custom["syntax"] = Item::Table(syntax);
    let mut markdown = Table::new();
    for (key, color) in [
        ("text", "white"),
        ("heading", "light-blue"),
        ("link", "light-magenta"),
        ("link_url", "gray"),
        ("code", "light-cyan"),
        ("punctuation", "gray"),
        ("marker", "light-cyan"),
    ] {
        markdown[key] = value(color);
    }
    custom["markdown"] = Item::Table(markdown);
    let defaults = RenderedTheme::defaults(true);
    let mut rendered = Table::new();
    rendered.decor_mut().set_prefix("\n# Colors use #RRGGBB. Fonts use bundled or a local TTF/OTF path relative to this file.\n# Sizes and padding use pixels; scales and line height use percent.\n");
    for (key, color) in [
        ("background", &defaults.background),
        ("text", &defaults.text),
        ("heading", &defaults.heading),
        ("link", &defaults.link),
        ("code_background", &defaults.code_background),
        ("code_text", &defaults.code_text),
        ("quote", &defaults.quote),
        ("quote_bar", &defaults.quote_bar),
        ("table_border", &defaults.table_border),
        ("table_stripe", &defaults.table_stripe),
    ] {
        rendered[key] = value(color.as_str());
    }
    for key in ["body_font", "heading_font", "code_font"] {
        rendered[key] = value("bundled");
    }
    for (key, number) in [
        ("body_size_px", defaults.body_size_px),
        ("code_size_px", defaults.code_size_px),
        ("heading_scale_percent", defaults.heading_scale_percent),
        ("line_height_percent", defaults.line_height_percent),
        ("page_padding_px", defaults.page_padding_px),
    ] {
        rendered[key] = value(i64::from(number));
    }
    custom["rendered"] = Item::Table(rendered);
    themes[name] = Item::Table(custom);

    write_document(path, &document)
}

fn read_document_or_new(path: &Path) -> io::Result<DocumentMut> {
    match fs::read_to_string(path) {
        Ok(contents) => parse_document(&contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // Carry a theme saved in the legacy plain-text file forward, since
            // creating config.toml stops that file from being read.
            let mut document = DocumentMut::new();
            if let Some(name) = load_legacy_theme(path)? {
                let mut theme = Table::new();
                theme["selected"] = value(name);
                document["theme"] = Item::Table(theme);
            }
            Ok(document)
        }
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

fn parse_custom_theme(
    name: &str,
    table: &Table,
    built_ins: &[Theme],
    config_dir: &Path,
) -> io::Result<Theme> {
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

    if let Some(syntax) = nested_table(table, "syntax", name)? {
        for (key, target) in [
            ("text", &mut palette.syntax.text),
            ("comment", &mut palette.syntax.comment),
            ("keyword", &mut palette.syntax.keyword),
            ("string", &mut palette.syntax.string),
            ("number", &mut palette.syntax.number),
            ("constant", &mut palette.syntax.constant),
            ("entity", &mut palette.syntax.entity),
            ("support", &mut palette.syntax.support),
        ] {
            apply_color(syntax, key, name, target)?;
        }
    }
    if let Some(markdown) = nested_table(table, "markdown", name)? {
        for (key, target) in [
            ("text", &mut palette.markdown.text),
            ("heading", &mut palette.markdown.heading),
            ("link", &mut palette.markdown.link),
            ("link_url", &mut palette.markdown.link_url),
            ("code", &mut palette.markdown.code),
            ("punctuation", &mut palette.markdown.punctuation),
            ("marker", &mut palette.markdown.marker),
        ] {
            apply_color(markdown, key, name, target)?;
        }
    }
    let mut rendered = (*base.rendered).clone();
    if let Some(settings) = nested_table(table, "rendered", name)? {
        for (key, item) in settings {
            macro_rules! string_field {
                ($field:ident) => {{
                    rendered.$field = item
                        .as_str()
                        .ok_or_else(|| {
                            invalid_data(format!(
                                "rendered.{key} in custom theme {name:?} must be a string"
                            ))
                        })?
                        .to_owned();
                }};
            }
            macro_rules! font_field {
                ($field:ident) => {{
                    let font = item.as_str().ok_or_else(|| {
                        invalid_data(format!(
                            "rendered.{key} in custom theme {name:?} must be a font path"
                        ))
                    })?;
                    rendered.$field = if font == "bundled" {
                        None
                    } else {
                        Some(PathBuf::from(font))
                    };
                }};
            }
            macro_rules! number_field {
                ($field:ident) => {{
                    let number = item.as_integer().and_then(|n| u16::try_from(n).ok())
                        .ok_or_else(|| invalid_data(format!("rendered.{key} in custom theme {name:?} must be a nonnegative integer")))?;
                    rendered.$field = number;
                }};
            }
            match key {
                "background" => string_field!(background),
                "text" => string_field!(text),
                "heading" => string_field!(heading),
                "link" => string_field!(link),
                "code_background" => string_field!(code_background),
                "code_text" => string_field!(code_text),
                "quote" => string_field!(quote),
                "quote_bar" => string_field!(quote_bar),
                "table_border" => string_field!(table_border),
                "table_stripe" => string_field!(table_stripe),
                "body_font" => font_field!(body_font),
                "heading_font" => font_field!(heading_font),
                "code_font" => font_field!(code_font),
                "body_size_px" => number_field!(body_size_px),
                "code_size_px" => number_field!(code_size_px),
                "heading_scale_percent" => number_field!(heading_scale_percent),
                "line_height_percent" => number_field!(line_height_percent),
                "page_padding_px" => number_field!(page_padding_px),
                _ => {
                    return Err(invalid_data(format!(
                        "unknown rendered setting {key:?} in custom theme {name:?}"
                    )));
                }
            }
        }
    }
    let rendered = rendered.resolve(config_dir).map_err(|error| {
        invalid_data(format!(
            "rendered settings in custom theme {name:?}: {error}"
        ))
    })?;
    Ok(Theme {
        name: name.to_owned(),
        palette,
        rendered,
    })
}

fn nested_table<'a>(table: &'a Table, key: &str, name: &str) -> io::Result<Option<&'a Table>> {
    table
        .get(key)
        .map(|item| {
            item.as_table().ok_or_else(|| {
                invalid_data(format!("{key:?} in custom theme {name:?} must be a table"))
            })
        })
        .transpose()
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
                syntax: EMPTY_SYNTAX,
                markdown: EMPTY_MARKDOWN,
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
                syntax: EMPTY_SYNTAX,
                markdown: EMPTY_MARKDOWN,
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
                syntax: EMPTY_SYNTAX,
                markdown: EMPTY_MARKDOWN,
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
        syntax: EMPTY_SYNTAX,
        markdown: EMPTY_MARKDOWN,
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
        syntax: EMPTY_SYNTAX,
        markdown: EMPTY_MARKDOWN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn editor_save_preserves_sparse_values_and_unrelated_config() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let contents = r##"# file comment
[defaults]
hidden = true # keep this

[themes.mine]
extends = "light" # base
text = "default" # explicit terminal reset

[themes.mine.syntax]
keyword = "red" # source comment

[themes.other]
extends = "dark" # another theme
"##;
        fs::write(&path, contents).unwrap();
        let mut draft = load_custom_theme_at(&path, "MINE").unwrap();
        draft["accent"] = value("#123456");
        save_custom_theme_at(&path, "mine", &draft, false).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        for expected in [
            "# file comment",
            "hidden = true # keep this",
            "extends = \"light\" # base",
            "text = \"default\" # explicit terminal reset",
            "keyword = \"red\" # source comment",
            "extends = \"dark\" # another theme",
        ] {
            assert!(saved.contains(expected), "missing {expected:?}: {saved}");
        }
        let config = Config::from_toml_at(&saved, dir.path()).unwrap();
        let theme = config.find("mine").unwrap();
        assert_eq!(theme.palette().text, Color::Reset);
        assert_eq!(theme.palette().accent, Color::Rgb(0x12, 0x34, 0x56));
        assert_eq!(
            theme.palette().muted,
            config.find("light").unwrap().palette().muted
        );
        assert!(!saved.contains("muted ="));
    }

    #[test]
    fn editor_rejects_invalid_drafts_and_duplicate_names_without_writing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "[themes.mine]\nextends = \"dark\"\n";
        fs::write(&path, original).unwrap();
        let mut draft = load_custom_theme_at(&path, "mine").unwrap();
        draft["accent"] = value("not-a-color");
        assert!(preview_custom_theme_at(&path, "mine", &draft).is_err());
        assert!(save_custom_theme_at(&path, "mine", &draft, false).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        let draft = new_custom_theme("light").unwrap();
        assert_eq!(
            save_custom_theme_at(&path, "MINE", &draft, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            save_custom_theme_at(&path, "dark", &draft, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            save_custom_theme_at(&path, "missing", &draft, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn editor_copy_inherits_complete_builtin_and_resolves_relative_fonts() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let draft = new_custom_theme("auto").unwrap();
        let preview = preview_custom_theme_at(&path, "copy", &draft).unwrap();
        let base = Config::built_ins().find("auto").unwrap().clone();
        assert_eq!(preview.palette(), base.palette());
        assert_eq!(*preview.rendered(), *base.rendered());
        save_custom_theme_at(&path, "copy", &draft, true).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("extends = \"auto\""));
        assert!(!saved.contains("text ="));
        let mut invalid = draft;
        let mut rendered = Table::new();
        rendered["body_font"] = value("missing.ttf");
        invalid["rendered"] = Item::Table(rendered);
        let error = preview_custom_theme_at(&path, "copy", &invalid)
            .unwrap_err()
            .to_string();
        assert!(error.contains(&dir.path().join("missing.ttf").display().to_string()));
        assert!(save_custom_theme_at(&path, "copy", &invalid, false).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), saved);
    }

    #[test]
    fn custom_roles_are_independent_and_inherit_light_rendering() {
        let config = Config::from_toml(
            r##"
[themes.day]
extends = "light"
accent = "#010203"
[themes.day.syntax]
keyword = "#112233"
[themes.day.markdown]
heading = "#445566"
[themes.day.rendered]
link = "#abcdef"
body_size_px = 20
"##,
        )
        .unwrap();
        let base = config.find("light").unwrap();
        let custom = config.find("day").unwrap();
        assert_eq!(
            custom.palette().syntax.keyword,
            Color::Rgb(0x11, 0x22, 0x33)
        );
        assert_eq!(custom.palette().syntax.string, base.palette().syntax.string);
        assert_eq!(
            custom.palette().markdown.heading,
            Color::Rgb(0x44, 0x55, 0x66)
        );
        assert_eq!(custom.palette().markdown.link, base.palette().markdown.link);
        assert!(!custom.rendered().dark);
        assert_eq!(custom.rendered().background, base.rendered().background);
        assert_eq!(custom.rendered().link, "#abcdef");
        assert_eq!(custom.rendered().body_size_px, 20);
    }

    #[test]
    fn rendered_errors_identify_setting_and_config_relative_font() {
        let dir = tempdir().unwrap();
        let config = "[themes.x.rendered]\nbody_font = \"fonts/missing.ttf\"\n";
        let error = Config::from_toml_at(config, dir.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("body_font"));
        assert!(error.contains(&dir.path().join("fonts/missing.ttf").display().to_string()));
        for (setting, value) in [
            ("body_size_px", "8"),
            ("line_height_percent", "401"),
            ("background", "\"red\""),
            ("body_font_typo", "\"bundled\""),
        ] {
            let config = format!("[themes.x.rendered]\n{setting} = {value}\n");
            let error = Config::from_toml_at(&config, dir.path())
                .unwrap_err()
                .to_string();
            assert!(error.contains(setting), "{error}");
        }
    }

    #[test]
    fn generated_theme_contains_editable_roles() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        create_custom_theme_at(&path, "sample").unwrap();
        let contents = fs::read_to_string(path).unwrap();
        assert!(contents.contains("[themes.sample.syntax]"));
        assert!(contents.contains("[themes.sample.markdown]"));
        assert!(contents.contains("[themes.sample.rendered]"));
        assert!(contents.contains("body_font = \"bundled\""));
        Config::from_toml_at(&contents, dir.path()).unwrap();
    }

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
    fn saving_defaults_keeps_a_legacy_theme_and_comments() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("de/config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "# mine\ntheme = \"ocean\" # inline\n").unwrap();

        let defaults = BrowseDefaults {
            show_hidden: true,
            sort_mode: crate::SortMode::Modified,
            sort_direction: crate::SortDirection::Descending,
        };
        save_defaults_at(&path, defaults).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# mine"));
        assert!(
            saved.contains("theme = \"ocean\" # inline"),
            "saved config:\n{saved}"
        );

        let config = Config::from_toml(&saved).unwrap();
        assert_eq!(config.saved_theme(), Some("ocean"));
        assert_eq!(config.defaults(), defaults);
    }

    #[test]
    fn creating_config_carries_forward_the_legacy_theme_file() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("de/config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path.with_file_name("theme"), "dark\n").unwrap();

        save_defaults_at(&path, BrowseDefaults::default()).unwrap();
        let config = Config::from_toml(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.saved_theme(), Some("dark"));
    }

    #[test]
    fn ensuring_the_config_file_carries_forward_the_legacy_theme_file() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("de/config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path.with_file_name("theme"), "dark\n").unwrap();

        ensure_config_file_at(&path).unwrap();
        let config = Config::from_toml(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.saved_theme(), Some("dark"));
    }

    #[test]
    fn ensuring_the_config_file_leaves_an_existing_file_alone() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("de/config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "# exactly this\n").unwrap();

        ensure_config_file_at(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "# exactly this\n");
    }

    #[test]
    fn saving_settings_writes_both_tables_and_keeps_the_rest() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("de/config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "# mine\ntheme = \"ocean\"\n\n[display]\ndate_format = \"custom\"\ncustom_format = \"%Y\"\n",
        )
        .unwrap();

        let defaults = BrowseDefaults {
            show_hidden: true,
            ..BrowseDefaults::default()
        };
        let mut display = Config::from_toml(&fs::read_to_string(&path).unwrap())
            .unwrap()
            .display()
            .clone();
        display.set_date_format(crate::DateFormat::Us);
        save_settings_at(&path, defaults, &display).unwrap();

        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# mine"));
        assert!(saved.contains("custom_format = \"%Y\""));
        let config = Config::from_toml(&saved).unwrap();
        assert_eq!(config.saved_theme(), Some("ocean"));
        assert_eq!(config.defaults(), defaults);
        assert_eq!(config.display(), &display);
        assert_eq!(config.display().custom_format(), Some("%Y"));
    }

    #[test]
    fn saving_defaults_creates_a_missing_config() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("de/config.toml");

        save_defaults_at(&path, BrowseDefaults::default()).unwrap();
        let config = Config::from_toml(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.defaults(), BrowseDefaults::default());
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
