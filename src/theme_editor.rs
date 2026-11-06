//! Editable, sparse custom-theme overrides and their resolved preview.

use std::io;

use toml_edit::{Item, Table, value};

use crate::theme::{Theme, preview_custom_theme};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldKind {
    Base,
    Color,
    RenderedColor,
    Bool,
    Font,
    Number,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Field {
    pub group: &'static str,
    pub key: &'static str,
    pub kind: FieldKind,
}

// Keep this list in the same order as the configuration's logical sections.
pub static FIELDS: &[Field] = &[
    Field {
        group: "Base",
        key: "extends",
        kind: FieldKind::Base,
    },
    Field {
        group: "Interface",
        key: "accent",
        kind: FieldKind::Color,
    },
    Field {
        group: "Interface",
        key: "text",
        kind: FieldKind::Color,
    },
    Field {
        group: "Interface",
        key: "muted",
        kind: FieldKind::Color,
    },
    Field {
        group: "Interface",
        key: "title",
        kind: FieldKind::Color,
    },
    Field {
        group: "Interface",
        key: "error",
        kind: FieldKind::Color,
    },
    Field {
        group: "Interface",
        key: "symlink",
        kind: FieldKind::Color,
    },
    Field {
        group: "Interface",
        key: "selection_fg",
        kind: FieldKind::Color,
    },
    Field {
        group: "Interface",
        key: "selection_bg",
        kind: FieldKind::Color,
    },
    Field {
        group: "Interface",
        key: "reverse_selection",
        kind: FieldKind::Bool,
    },
    Field {
        group: "Interface",
        key: "dim_muted",
        kind: FieldKind::Bool,
    },
    Field {
        group: "Syntax",
        key: "text",
        kind: FieldKind::Color,
    },
    Field {
        group: "Syntax",
        key: "comment",
        kind: FieldKind::Color,
    },
    Field {
        group: "Syntax",
        key: "keyword",
        kind: FieldKind::Color,
    },
    Field {
        group: "Syntax",
        key: "string",
        kind: FieldKind::Color,
    },
    Field {
        group: "Syntax",
        key: "number",
        kind: FieldKind::Color,
    },
    Field {
        group: "Syntax",
        key: "constant",
        kind: FieldKind::Color,
    },
    Field {
        group: "Syntax",
        key: "entity",
        kind: FieldKind::Color,
    },
    Field {
        group: "Syntax",
        key: "support",
        kind: FieldKind::Color,
    },
    Field {
        group: "Markdown",
        key: "text",
        kind: FieldKind::Color,
    },
    Field {
        group: "Markdown",
        key: "heading",
        kind: FieldKind::Color,
    },
    Field {
        group: "Markdown",
        key: "link",
        kind: FieldKind::Color,
    },
    Field {
        group: "Markdown",
        key: "link_url",
        kind: FieldKind::Color,
    },
    Field {
        group: "Markdown",
        key: "code",
        kind: FieldKind::Color,
    },
    Field {
        group: "Markdown",
        key: "punctuation",
        kind: FieldKind::Color,
    },
    Field {
        group: "Markdown",
        key: "marker",
        kind: FieldKind::Color,
    },
    Field {
        group: "Rendered colors",
        key: "background",
        kind: FieldKind::RenderedColor,
    },
    Field {
        group: "Rendered colors",
        key: "text",
        kind: FieldKind::RenderedColor,
    },
    Field {
        group: "Rendered colors",
        key: "heading",
        kind: FieldKind::RenderedColor,
    },
    Field {
        group: "Rendered colors",
        key: "link",
        kind: FieldKind::RenderedColor,
    },
    Field {
        group: "Rendered colors",
        key: "code_background",
        kind: FieldKind::RenderedColor,
    },
    Field {
        group: "Rendered colors",
        key: "code_text",
        kind: FieldKind::RenderedColor,
    },
    Field {
        group: "Rendered colors",
        key: "quote",
        kind: FieldKind::RenderedColor,
    },
    Field {
        group: "Rendered colors",
        key: "quote_bar",
        kind: FieldKind::RenderedColor,
    },
    Field {
        group: "Rendered colors",
        key: "table_border",
        kind: FieldKind::RenderedColor,
    },
    Field {
        group: "Rendered colors",
        key: "table_stripe",
        kind: FieldKind::RenderedColor,
    },
    Field {
        group: "Rendered typography",
        key: "body_font",
        kind: FieldKind::Font,
    },
    Field {
        group: "Rendered typography",
        key: "heading_font",
        kind: FieldKind::Font,
    },
    Field {
        group: "Rendered typography",
        key: "code_font",
        kind: FieldKind::Font,
    },
    Field {
        group: "Rendered typography",
        key: "body_size_px",
        kind: FieldKind::Number,
    },
    Field {
        group: "Rendered typography",
        key: "code_size_px",
        kind: FieldKind::Number,
    },
    Field {
        group: "Rendered typography",
        key: "heading_scale_percent",
        kind: FieldKind::Number,
    },
    Field {
        group: "Rendered typography",
        key: "line_height_percent",
        kind: FieldKind::Number,
    },
    Field {
        group: "Rendered typography",
        key: "page_padding_px",
        kind: FieldKind::Number,
    },
];

#[derive(Clone, Debug)]
pub struct ThemeEditorDraft {
    name: String,
    table: Table,
    preview: Theme,
}

impl ThemeEditorDraft {
    pub fn new(name: String, table: Table) -> io::Result<Self> {
        let preview = preview_custom_theme(&name, &table)?;
        Ok(Self {
            name,
            table,
            preview,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn table(&self) -> &Table {
        &self.table
    }
    pub fn preview(&self) -> &Theme {
        &self.preview
    }

    /// Return the effective color for a swatch, including inherited colors.
    pub fn color(&self, index: usize) -> Option<ratatui::style::Color> {
        use ratatui::style::Color;
        let field = FIELDS.get(index)?;
        let palette = self.preview.palette();
        let color = match (field.group, field.key) {
            ("Interface", "accent") => palette.accent,
            ("Interface", "text") => palette.text,
            ("Interface", "muted") => palette.muted,
            ("Interface", "title") => palette.title,
            ("Interface", "error") => palette.error,
            ("Interface", "symlink") => palette.symlink,
            ("Interface", "selection_fg") => palette.emphasis_foreground,
            ("Interface", "selection_bg") => palette.emphasis_background,
            ("Syntax", "text") => palette.syntax.text,
            ("Syntax", "comment") => palette.syntax.comment,
            ("Syntax", "keyword") => palette.syntax.keyword,
            ("Syntax", "string") => palette.syntax.string,
            ("Syntax", "number") => palette.syntax.number,
            ("Syntax", "constant") => palette.syntax.constant,
            ("Syntax", "entity") => palette.syntax.entity,
            ("Syntax", "support") => palette.syntax.support,
            ("Markdown", "text") => palette.markdown.text,
            ("Markdown", "heading") => palette.markdown.heading,
            ("Markdown", "link") => palette.markdown.link,
            ("Markdown", "link_url") => palette.markdown.link_url,
            ("Markdown", "code") => palette.markdown.code,
            ("Markdown", "punctuation") => palette.markdown.punctuation,
            ("Markdown", "marker") => palette.markdown.marker,
            ("Rendered colors", key) => {
                let rendered = self.preview.rendered();
                let color = match key {
                    "background" => &rendered.background,
                    "text" => &rendered.text,
                    "heading" => &rendered.heading,
                    "link" => &rendered.link,
                    "code_background" => &rendered.code_background,
                    "code_text" => &rendered.code_text,
                    "quote" => &rendered.quote,
                    "quote_bar" => &rendered.quote_bar,
                    "table_border" => &rendered.table_border,
                    "table_stripe" => &rendered.table_stripe,
                    _ => return None,
                };
                let hex = color.strip_prefix('#')?;
                if hex.len() != 6 || !hex.is_ascii() {
                    return None;
                }
                return Some(Color::Rgb(
                    u8::from_str_radix(&hex[0..2], 16).ok()?,
                    u8::from_str_radix(&hex[2..4], 16).ok()?,
                    u8::from_str_radix(&hex[4..6], 16).ok()?,
                ));
            }
            _ => return None,
        };
        Some(color)
    }

    /// Return the resolved value shown in the preview.
    pub fn effective_value(&self, index: usize) -> String {
        use ratatui::style::Color;

        let Some(field) = FIELDS.get(index) else {
            return String::new();
        };
        if field.kind == FieldKind::Base {
            let value = self.input(index);
            return if value.is_empty() {
                "auto".into()
            } else {
                value
            };
        }
        if field.kind == FieldKind::RenderedColor {
            let rendered = self.preview.rendered();
            return match field.key {
                "background" => &rendered.background,
                "text" => &rendered.text,
                "heading" => &rendered.heading,
                "link" => &rendered.link,
                "code_background" => &rendered.code_background,
                "code_text" => &rendered.code_text,
                "quote" => &rendered.quote,
                "quote_bar" => &rendered.quote_bar,
                "table_border" => &rendered.table_border,
                "table_stripe" => &rendered.table_stripe,
                _ => return String::new(),
            }
            .clone();
        }
        if field.kind == FieldKind::Color {
            return match self.color(index) {
                Some(Color::Reset) => "default".into(),
                Some(Color::Rgb(r, g, b)) => format!("#{r:02x}{g:02x}{b:02x}"),
                Some(Color::Black) => "black".into(),
                Some(Color::Red) => "red".into(),
                Some(Color::Green) => "green".into(),
                Some(Color::Yellow) => "yellow".into(),
                Some(Color::Blue) => "blue".into(),
                Some(Color::Magenta) => "magenta".into(),
                Some(Color::Cyan) => "cyan".into(),
                Some(Color::Gray) => "gray".into(),
                Some(Color::DarkGray) => "dark-gray".into(),
                Some(Color::LightRed) => "light-red".into(),
                Some(Color::LightGreen) => "light-green".into(),
                Some(Color::LightYellow) => "light-yellow".into(),
                Some(Color::LightBlue) => "light-blue".into(),
                Some(Color::LightMagenta) => "light-magenta".into(),
                Some(Color::LightCyan) => "light-cyan".into(),
                Some(Color::White) => "white".into(),
                Some(Color::Indexed(n)) => n.to_string(),
                None => String::new(),
            };
        }
        let palette = self.preview.palette();
        let rendered = self.preview.rendered();
        match field.key {
            "reverse_selection" => return palette.reverse_emphasis.to_string(),
            "dim_muted" => return palette.dim_muted.to_string(),
            "body_font" => rendered.body_font.as_deref(),
            "heading_font" => rendered.heading_font.as_deref(),
            "code_font" => rendered.code_font.as_deref(),
            "body_size_px" => return rendered.body_size_px.to_string(),
            "code_size_px" => return rendered.code_size_px.to_string(),
            "heading_scale_percent" => return rendered.heading_scale_percent.to_string(),
            "line_height_percent" => return rendered.line_height_percent.to_string(),
            "page_padding_px" => return rendered.page_padding_px.to_string(),
            _ => return String::new(),
        }
        .map_or_else(
            || "bundled".into(),
            |path| path.to_string_lossy().into_owned(),
        )
    }

    /// Whether this field has no explicit override.
    pub fn inherited(&self, index: usize) -> bool {
        FIELDS
            .get(index)
            .is_some_and(|field| field_item(&self.table, field).is_none())
    }

    /// Describe the field's purpose and accepted values.
    pub fn guidance(&self, index: usize) -> &'static str {
        let Some(field) = FIELDS.get(index) else {
            return "";
        };
        match (field.group, field.key) {
            ("Base", _) => "Base: auto, light, dark, mono, ocean, forest, amber, rose.",
            ("Interface", "reverse_selection") => "Reverse selected item colors: true or false.",
            ("Interface", "dim_muted") => "Dim muted interface text: true or false.",
            ("Rendered typography", "body_font") => {
                "Body font: local TTF/OTF path relative to the config file; empty inherits."
            }
            ("Rendered typography", "heading_font") => {
                "Heading font: local TTF/OTF path relative to the config file; empty inherits."
            }
            ("Rendered typography", "code_font") => {
                "Code font: local TTF/OTF path relative to the config file; empty inherits."
            }
            ("Rendered typography", "body_size_px") => {
                "Body font size in pixels: integer from 9 to 32."
            }
            ("Rendered typography", "code_size_px") => {
                "Code font size in pixels: integer from 9 to 32."
            }
            ("Rendered typography", "heading_scale_percent") => {
                "Heading scale in percent: integer from 50 to 200."
            }
            ("Rendered typography", "line_height_percent") => {
                "Line height in percent: integer from 100 to 400."
            }
            ("Rendered typography", "page_padding_px") => {
                "Page padding in pixels: integer from 0 to 128."
            }
            ("Rendered colors", _) => "Rendered Markdown color: #RRGGBB, for example #123456.",
            ("Interface", _) => "Interface color: #123456, a name such as red, or default.",
            ("Syntax", _) => "Syntax color: #123456, a name such as red, or default.",
            ("Markdown", _) => "Markdown source color: #123456, a name such as red, or default.",
            _ => "",
        }
    }

    /// The explicit value, or an empty string when the value is inherited.
    pub fn input(&self, index: usize) -> String {
        let Some(field) = FIELDS.get(index) else {
            return String::new();
        };
        let Some(item) = field_item(&self.table, field) else {
            return String::new();
        };
        if let Some(text) = item.as_str() {
            return text.to_owned();
        }
        if let Some(boolean) = item.as_bool() {
            return boolean.to_string();
        }
        if let Some(number) = item.as_integer() {
            return number.to_string();
        }
        String::new()
    }

    /// Validate an edit before changing either the sparse draft or its preview.
    /// An empty input removes the override and restores inheritance.
    pub fn commit(&mut self, index: usize, input: &str) -> io::Result<()> {
        self.change(index, Some(input), preview_custom_theme)
    }

    pub fn reset(&mut self, index: usize) -> io::Result<()> {
        self.change(index, None, preview_custom_theme)
    }

    fn change(
        &mut self,
        index: usize,
        input: Option<&str>,
        resolve: impl FnOnce(&str, &Table) -> io::Result<Theme>,
    ) -> io::Result<()> {
        let field = FIELDS
            .get(index)
            .ok_or_else(|| invalid("unknown theme field"))?;
        let mut next = self.table.clone();
        let input = input.filter(|text| !text.is_empty());
        if let Some(input) = input {
            let mut item = match field.kind {
                FieldKind::Bool => value(
                    input
                        .parse::<bool>()
                        .map_err(|_| invalid("value must be true or false"))?,
                ),
                FieldKind::Number => value(
                    input
                        .parse::<i64>()
                        .map_err(|_| invalid("value must be an integer"))?,
                ),
                _ => value(input),
            };
            let table = field_table_mut(&mut next, field);
            if let (Some(old), Some(new)) = (
                table.get(field.key).and_then(Item::as_value),
                item.as_value_mut(),
            ) {
                *new.decor_mut() = old.decor().clone();
            }
            table.insert(field.key, item);
        } else if let Some(table) = existing_field_table_mut(&mut next, field) {
            table.remove(field.key);
        }
        let preview = resolve(&self.name, &next)?;
        self.table = next;
        self.preview = preview;
        Ok(())
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn section(field: &Field) -> Option<&'static str> {
    match field.group {
        "Syntax" => Some("syntax"),
        "Markdown" => Some("markdown"),
        "Rendered colors" | "Rendered typography" => Some("rendered"),
        _ => None,
    }
}

fn field_item<'a>(table: &'a Table, field: &Field) -> Option<&'a Item> {
    let table = match section(field) {
        Some(section) => table.get(section)?.as_table()?,
        None => table,
    };
    table.get(field.key)
}

fn existing_field_table_mut<'a>(table: &'a mut Table, field: &Field) -> Option<&'a mut Table> {
    match section(field) {
        Some(section) => table.get_mut(section)?.as_table_mut(),
        None => Some(table),
    }
}

fn field_table_mut<'a>(table: &'a mut Table, field: &Field) -> &'a mut Table {
    match section(field) {
        Some(section) => {
            if !table.contains_key(section) {
                table.insert(section, Item::Table(Table::new()));
            }
            table
                .get_mut(section)
                .and_then(Item::as_table_mut)
                .expect("validated theme section")
        }
        None => table,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::preview_custom_theme_at;
    use ratatui::style::Color;
    use std::path::Path;
    use tempfile::tempdir;
    use toml_edit::DocumentMut;

    fn index(group: &str, key: &str) -> usize {
        FIELDS
            .iter()
            .position(|field| field.group == group && field.key == key)
            .unwrap()
    }

    fn draft(path: &Path, table: Table) -> ThemeEditorDraft {
        let name = "example".to_owned();
        let preview = preview_custom_theme_at(path, &name, &table).unwrap();
        ThemeEditorDraft {
            name,
            table,
            preview,
        }
    }

    fn change(
        draft: &mut ThemeEditorDraft,
        path: &Path,
        group: &str,
        key: &str,
        input: Option<&str>,
    ) -> io::Result<()> {
        draft.change(index(group, key), input, |name, table| {
            preview_custom_theme_at(path, name, table)
        })
    }

    #[test]
    fn fields_cover_all_theme_settings_in_sections() {
        let groups = [
            ("Base", "extends"),
            (
                "Interface",
                "accent text muted title error symlink selection_fg selection_bg reverse_selection dim_muted",
            ),
            (
                "Syntax",
                "text comment keyword string number constant entity support",
            ),
            (
                "Markdown",
                "text heading link link_url code punctuation marker",
            ),
            (
                "Rendered colors",
                "background text heading link code_background code_text quote quote_bar table_border table_stripe",
            ),
            (
                "Rendered typography",
                "body_font heading_font code_font body_size_px code_size_px heading_scale_percent line_height_percent page_padding_px",
            ),
        ];
        let expected: Vec<_> = groups
            .iter()
            .flat_map(|(group, keys)| keys.split_whitespace().map(move |key| (*group, key)))
            .collect();
        let actual: Vec<_> = FIELDS
            .iter()
            .map(|field| (field.group, field.key))
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn effective_values_follow_preview_and_preserve_sparse_input() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut draft = draft(&path, Table::new());
        for (i, field) in FIELDS.iter().enumerate() {
            assert!(draft.inherited(i), "{}.{}", field.group, field.key);
            assert_eq!(draft.input(i), "");
            assert!(
                !draft.effective_value(i).is_empty(),
                "{}.{}",
                field.group,
                field.key
            );
            assert!(
                !draft.guidance(i).is_empty(),
                "{}.{}",
                field.group,
                field.key
            );
        }
        assert_eq!(draft.effective_value(index("Base", "extends")), "auto");
        assert_eq!(
            draft.effective_value(index("Rendered typography", "body_font")),
            "bundled"
        );
        assert_eq!(
            draft.effective_value(index("Rendered typography", "body_size_px")),
            "16"
        );
        let accent = index("Interface", "accent");
        let original = draft.effective_value(accent);
        change(&mut draft, &path, "Interface", "accent", Some("#123456")).unwrap();
        assert!(!draft.inherited(accent));
        assert_eq!(draft.effective_value(accent), "#123456");
        change(&mut draft, &path, "Interface", "accent", None).unwrap();
        assert!(draft.inherited(accent));
        assert_eq!(draft.effective_value(accent), original);
        change(&mut draft, &path, "Syntax", "keyword", Some("reset")).unwrap();
        assert_eq!(draft.effective_value(index("Syntax", "keyword")), "default");
        change(&mut draft, &path, "Interface", "dim_muted", Some("true")).unwrap();
        assert_eq!(
            draft.effective_value(index("Interface", "dim_muted")),
            "true"
        );
        change(
            &mut draft,
            &path,
            "Rendered typography",
            "body_size_px",
            Some("20"),
        )
        .unwrap();
        assert_eq!(
            draft.effective_value(index("Rendered typography", "body_size_px")),
            "20"
        );
        change(
            &mut draft,
            &path,
            "Rendered colors",
            "link",
            Some("#ABCDEF"),
        )
        .unwrap();
        assert_eq!(
            draft.effective_value(index("Rendered colors", "link")),
            "#ABCDEF"
        );
        assert_eq!(draft.effective_value(FIELDS.len()), "");
        assert!(!draft.inherited(FIELDS.len()));
        assert_eq!(draft.guidance(FIELDS.len()), "");
    }

    #[test]
    fn effective_font_uses_resolved_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let font = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("vendor/franken-markdown/fmd-font/fonts/test-shaping/FmdShaping.ttf");
        let mut draft = draft(&path, Table::new());
        change(
            &mut draft,
            &path,
            "Rendered typography",
            "body_font",
            font.to_str(),
        )
        .unwrap();
        let i = index("Rendered typography", "body_font");
        assert_eq!(draft.effective_value(i), font.to_string_lossy());
        assert!(!draft.inherited(i));
    }

    #[test]
    fn edits_resolve_preview_and_reset_to_inheritance() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut table = Table::new();
        table.insert("extends", value("light"));
        let mut draft = draft(&path, table);
        let inherited = draft.preview().palette().accent;
        assert_eq!(draft.input(index("Interface", "accent")), "");
        change(&mut draft, &path, "Interface", "accent", Some("#123456")).unwrap();
        assert_eq!(draft.input(index("Interface", "accent")), "#123456");
        assert_eq!(
            draft.preview().palette().accent,
            Color::Rgb(0x12, 0x34, 0x56)
        );
        assert_eq!(
            draft.color(index("Interface", "accent")),
            Some(Color::Rgb(0x12, 0x34, 0x56))
        );
        change(&mut draft, &path, "Syntax", "keyword", Some("default")).unwrap();
        assert_eq!(draft.preview().palette().syntax.keyword, Color::Reset);
        assert_eq!(draft.color(index("Syntax", "keyword")), Some(Color::Reset));
        change(
            &mut draft,
            &path,
            "Rendered colors",
            "link",
            Some("#abcdef"),
        )
        .unwrap();
        assert_eq!(draft.preview().rendered().link, "#abcdef");
        assert_eq!(
            draft.color(index("Rendered colors", "link")),
            Some(Color::Rgb(0xab, 0xcd, 0xef))
        );
        assert_eq!(draft.color(index("Base", "extends")), None);
        change(&mut draft, &path, "Interface", "accent", None).unwrap();
        assert_eq!(draft.preview().palette().accent, inherited);
        assert_eq!(draft.color(index("Interface", "accent")), Some(inherited));
        assert_eq!(draft.input(index("Interface", "accent")), "");
        change(&mut draft, &path, "Syntax", "keyword", Some("")).unwrap();
        assert_eq!(draft.input(index("Syntax", "keyword")), "");
    }

    #[test]
    fn invalid_edits_leave_draft_and_preview_unchanged() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut draft = draft(&path, Table::new());
        let before = draft.table().to_string();
        let preview = draft.preview().clone();
        for (group, key, input) in [
            ("Base", "extends", "nonexistent"),
            ("Interface", "accent", "not-a-color"),
            ("Interface", "dim_muted", "yes"),
            ("Rendered colors", "background", "red"),
            ("Rendered typography", "body_size_px", "-1"),
            ("Rendered typography", "body_font", "missing-font.ttf"),
        ] {
            assert!(
                change(&mut draft, &path, group, key, Some(input)).is_err(),
                "{group}.{key}"
            );
            assert_eq!(draft.table().to_string(), before);
            assert_eq!(draft.preview(), &preview);
        }
    }

    #[test]
    fn edit_preserves_unrelated_values_and_comments() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let doc = "extends = \"dark\"\naccent = \"red\" # keep\n[syntax]\nkeyword = \"blue\"\n"
            .parse::<DocumentMut>()
            .unwrap();
        let mut draft = draft(&path, doc.as_table().clone());
        change(&mut draft, &path, "Interface", "accent", Some("green")).unwrap();
        let text = draft.table().to_string();
        assert!(text.contains("# keep"));
        assert_eq!(draft.input(index("Syntax", "keyword")), "blue");
        change(&mut draft, &path, "Interface", "dim_muted", Some("true")).unwrap();
        assert!(draft.preview().palette().dim_muted);
        change(
            &mut draft,
            &path,
            "Rendered typography",
            "body_size_px",
            Some("20"),
        )
        .unwrap();
        assert_eq!(draft.preview().rendered().body_size_px, 20);
    }
}
