use crate::{SortDirection, SortMode};
use chrono::format::{Item, StrftimeItems};
use std::io;
use toml_edit::{DocumentMut, Item as TomlItem, Table, value};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DateFormat {
    Iso,
    Us,
    European,
    Relative,
    Custom(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeFormat {
    TwelveHour,
    TwentyFourHour,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Timezone {
    Local,
    Utc,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplaySettings {
    modified: bool,
    date_format: DateFormat,
    time_format: TimeFormat,
    timezone: Timezone,
    /// A valid `custom_format` from the file, kept even while another date
    /// format is selected so the settings picker can switch back to it.
    custom_format: Option<String>,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            modified: true,
            date_format: DateFormat::Iso,
            time_format: TimeFormat::TwentyFourHour,
            timezone: Timezone::Local,
            custom_format: None,
        }
    }
}

impl DisplaySettings {
    pub(crate) fn from_document(document: &DocumentMut) -> io::Result<Self> {
        let Some(item) = document.get("display") else {
            return Ok(Self::default());
        };
        let table = item
            .as_table()
            .ok_or_else(|| invalid_data("display must be a table"))?;

        let modified = optional_bool(table, "display", "modified")?.unwrap_or(true);
        let time_format = match optional_string(table, "display", "time_format")?.unwrap_or("24h") {
            "12h" => TimeFormat::TwelveHour,
            "24h" => TimeFormat::TwentyFourHour,
            value => {
                return Err(invalid_data(format!(
                    "unknown display.time_format {value:?}; use 12h or 24h"
                )));
            }
        };
        let timezone = match optional_string(table, "display", "timezone")?.unwrap_or("local") {
            "local" => Timezone::Local,
            "utc" => Timezone::Utc,
            value => {
                return Err(invalid_data(format!(
                    "unknown display.timezone {value:?}; use local or utc"
                )));
            }
        };
        let date_format = match optional_string(table, "display", "date_format")?.unwrap_or("iso") {
            "iso" => DateFormat::Iso,
            "us" => DateFormat::Us,
            "european" | "eu" => DateFormat::European,
            "relative" => DateFormat::Relative,
            "custom" => {
                let format =
                    optional_string(table, "display", "custom_format")?.ok_or_else(|| {
                        invalid_data("display.custom_format is required when date_format is custom")
                    })?;
                validate_custom_format(format)?;
                DateFormat::Custom(format.to_owned())
            }
            value => {
                return Err(invalid_data(format!(
                    "unknown display.date_format {value:?}; use iso, us, european, relative, or custom"
                )));
            }
        };

        let custom_format = match &date_format {
            DateFormat::Custom(format) => Some(format.clone()),
            _ => table
                .get("custom_format")
                .and_then(TomlItem::as_str)
                .filter(|format| validate_custom_format(format).is_ok())
                .map(str::to_owned),
        };

        Ok(Self {
            modified,
            date_format,
            time_format,
            timezone,
            custom_format,
        })
    }

    pub const fn shows_modified(&self) -> bool {
        self.modified
    }

    pub const fn date_format(&self) -> &DateFormat {
        &self.date_format
    }

    pub const fn time_format(&self) -> TimeFormat {
        self.time_format
    }

    pub const fn timezone(&self) -> Timezone {
        self.timezone
    }

    pub fn custom_format(&self) -> Option<&str> {
        self.custom_format.as_deref()
    }

    pub fn set_shows_modified(&mut self, modified: bool) {
        self.modified = modified;
    }

    pub fn set_date_format(&mut self, date_format: DateFormat) {
        if let DateFormat::Custom(format) = &date_format {
            self.custom_format = Some(format.clone());
        }
        self.date_format = date_format;
    }

    pub fn set_time_format(&mut self, time_format: TimeFormat) {
        self.time_format = time_format;
    }

    pub fn set_timezone(&mut self, timezone: Timezone) {
        self.timezone = timezone;
    }

    /// Write these values into `[display]`, keeping any other keys and comments.
    /// A custom date format keeps the file's `custom_format`, which is only
    /// added when missing.
    pub(crate) fn write_to(&self, document: &mut DocumentMut) -> io::Result<()> {
        let table = section_mut(document, "display")?;
        set_value(table, "modified", self.modified);
        set_value(table, "date_format", date_format_name(&self.date_format));
        set_value(table, "time_format", time_format_name(self.time_format));
        set_value(table, "timezone", timezone_name(self.timezone));
        if let DateFormat::Custom(format) = &self.date_format
            && !table.contains_key("custom_format")
        {
            set_value(table, "custom_format", format.as_str());
        }
        Ok(())
    }
}

/// The spellings used in config.toml, shared by parsing errors, saving, and
/// the settings picker.
pub const fn date_format_name(format: &DateFormat) -> &'static str {
    match format {
        DateFormat::Iso => "iso",
        DateFormat::Us => "us",
        DateFormat::European => "european",
        DateFormat::Relative => "relative",
        DateFormat::Custom(_) => "custom",
    }
}

pub const fn time_format_name(format: TimeFormat) -> &'static str {
    match format {
        TimeFormat::TwelveHour => "12h",
        TimeFormat::TwentyFourHour => "24h",
    }
}

pub const fn timezone_name(timezone: Timezone) -> &'static str {
    match timezone {
        Timezone::Local => "local",
        Timezone::Utc => "utc",
    }
}

pub const fn sort_mode_name(mode: SortMode) -> &'static str {
    match mode {
        SortMode::Name => "name",
        SortMode::Modified => "modified",
    }
}

pub const fn sort_direction_name(direction: SortDirection) -> &'static str {
    match direction {
        SortDirection::Ascending => "ascending",
        SortDirection::Descending => "descending",
    }
}

/// Starting state for the picker's in-session toggles, read from `[defaults]`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrowseDefaults {
    pub show_hidden: bool,
    pub sort_mode: SortMode,
    pub sort_direction: SortDirection,
}

impl Default for BrowseDefaults {
    fn default() -> Self {
        Self {
            show_hidden: false,
            sort_mode: SortMode::Name,
            sort_direction: SortDirection::Ascending,
        }
    }
}

impl BrowseDefaults {
    pub(crate) fn from_document(document: &DocumentMut) -> io::Result<Self> {
        let Some(item) = document.get("defaults") else {
            return Ok(Self::default());
        };
        let table = item
            .as_table()
            .ok_or_else(|| invalid_data("defaults must be a table"))?;

        let show_hidden = optional_bool(table, "defaults", "hidden")?.unwrap_or(false);
        let sort_mode = match optional_string(table, "defaults", "sort")?.unwrap_or("name") {
            "name" => SortMode::Name,
            "modified" => SortMode::Modified,
            value => {
                return Err(invalid_data(format!(
                    "unknown defaults.sort {value:?}; use name or modified"
                )));
            }
        };
        let sort_direction =
            match optional_string(table, "defaults", "order")?.unwrap_or("ascending") {
                "ascending" => SortDirection::Ascending,
                "descending" => SortDirection::Descending,
                value => {
                    return Err(invalid_data(format!(
                        "unknown defaults.order {value:?}; use ascending or descending"
                    )));
                }
            };

        Ok(Self {
            show_hidden,
            sort_mode,
            sort_direction,
        })
    }

    /// Write these values into `[defaults]`, keeping any other keys and comments.
    pub(crate) fn write_to(self, document: &mut DocumentMut) -> io::Result<()> {
        let table = section_mut(document, "defaults")?;
        set_value(table, "hidden", self.show_hidden);
        set_value(table, "sort", sort_mode_name(self.sort_mode));
        set_value(table, "order", sort_direction_name(self.sort_direction));
        Ok(())
    }
}

fn section_mut<'a>(document: &'a mut DocumentMut, name: &str) -> io::Result<&'a mut Table> {
    if !document.contains_key(name) {
        document[name] = TomlItem::Table(Table::new());
    }
    document[name]
        .as_table_mut()
        .ok_or_else(|| invalid_data(format!("{name} must be a table")))
}

/// Replace a value while keeping any comment written beside it.
fn set_value(table: &mut Table, key: &str, new: impl Into<toml_edit::Value>) {
    let mut new = new.into();
    if let Some(existing) = table.get_mut(key).and_then(TomlItem::as_value_mut) {
        *new.decor_mut() = existing.decor().clone();
        *existing = new;
    } else {
        table[key] = value(new);
    }
}

fn optional_string<'a>(table: &'a Table, section: &str, key: &str) -> io::Result<Option<&'a str>> {
    match table.get(key) {
        None => Ok(None),
        Some(item) => item
            .as_str()
            .map(Some)
            .ok_or_else(|| invalid_data(format!("{section}.{key} must be a string"))),
    }
}

fn optional_bool(table: &Table, section: &str, key: &str) -> io::Result<Option<bool>> {
    match table.get(key) {
        None => Ok(None),
        Some(item) => item
            .as_bool()
            .map(Some)
            .ok_or_else(|| invalid_data(format!("{section}.{key} must be true or false"))),
    }
}

fn validate_custom_format(format: &str) -> io::Result<()> {
    if format.is_empty() {
        return Err(invalid_data("display.custom_format cannot be empty"));
    }
    if StrftimeItems::new(format).any(|item| match item {
        Item::Error => true,
        Item::Literal(value) | Item::Space(value) => value.chars().any(char::is_control),
        _ => false,
    }) {
        return Err(invalid_data(format!(
            "display.custom_format contains an invalid directive or control character: {format:?}"
        )));
    }
    Ok(())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(contents: &str) -> io::Result<DisplaySettings> {
        let document = contents.parse::<DocumentMut>().unwrap();
        DisplaySettings::from_document(&document)
    }

    #[test]
    fn defaults_match_the_existing_display() {
        assert_eq!(parse("").unwrap(), DisplaySettings::default());
    }

    #[test]
    fn parses_all_display_preferences() {
        let settings = parse(
            "[display]\nmodified = false\ndate_format = \"us\"\ntime_format = \"12h\"\ntimezone = \"utc\"\n",
        )
        .unwrap();

        assert!(!settings.shows_modified());
        assert_eq!(settings.date_format(), &DateFormat::Us);
        assert_eq!(settings.time_format(), TimeFormat::TwelveHour);
        assert_eq!(settings.timezone(), Timezone::Utc);
    }

    #[test]
    fn accepts_relative_and_valid_custom_formats() {
        assert_eq!(
            parse("[display]\ndate_format = \"relative\"\n")
                .unwrap()
                .date_format(),
            &DateFormat::Relative
        );
        assert_eq!(
            parse(
                "[display]\ndate_format = \"custom\"\ncustom_format = \"%b %e, %Y at %l:%M %p\"\n",
            )
            .unwrap()
            .date_format(),
            &DateFormat::Custom("%b %e, %Y at %l:%M %p".into())
        );
    }

    fn parse_defaults(contents: &str) -> io::Result<BrowseDefaults> {
        let document = contents.parse::<DocumentMut>().unwrap();
        BrowseDefaults::from_document(&document)
    }

    #[test]
    fn browse_defaults_match_the_existing_startup_state() {
        assert_eq!(parse_defaults("").unwrap(), BrowseDefaults::default());
    }

    #[test]
    fn parses_browse_defaults() {
        let defaults = parse_defaults(
            "[defaults]\nhidden = true\nsort = \"modified\"\norder = \"descending\"\n",
        )
        .unwrap();

        assert_eq!(
            defaults,
            BrowseDefaults {
                show_hidden: true,
                sort_mode: SortMode::Modified,
                sort_direction: SortDirection::Descending,
            }
        );
    }

    #[test]
    fn rejects_invalid_browse_defaults() {
        for contents in [
            "defaults = true\n",
            "[defaults]\nhidden = \"yes\"\n",
            "[defaults]\nsort = \"size\"\n",
            "[defaults]\norder = \"up\"\n",
        ] {
            assert_eq!(
                parse_defaults(contents).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn written_browse_defaults_parse_back_and_keep_comments() {
        let mut document = "# mine\n[defaults]\n# keep\nhidden = false\n"
            .parse::<DocumentMut>()
            .unwrap();
        let defaults = BrowseDefaults {
            show_hidden: true,
            sort_mode: SortMode::Modified,
            sort_direction: SortDirection::Descending,
        };
        defaults.write_to(&mut document).unwrap();

        let contents = document.to_string();
        assert!(contents.contains("# mine"));
        assert!(contents.contains("# keep"));
        assert_eq!(parse_defaults(&contents).unwrap(), defaults);
    }

    #[test]
    fn written_display_settings_parse_back_and_keep_custom_format_and_comments() {
        let mut document = "[display]\ndate_format = \"custom\" # mine\ncustom_format = \"%Y\"\n"
            .parse::<DocumentMut>()
            .unwrap();
        let mut settings = parse(&document.to_string()).unwrap();
        settings.set_date_format(DateFormat::Relative);
        settings.set_time_format(TimeFormat::TwelveHour);
        settings.set_timezone(Timezone::Utc);
        settings.set_shows_modified(false);
        settings.write_to(&mut document).unwrap();

        let contents = document.to_string();
        assert!(
            contents.contains("date_format = \"relative\" # mine"),
            "{contents}"
        );
        assert!(contents.contains("custom_format = \"%Y\""));
        assert_eq!(parse(&contents).unwrap(), settings);
    }

    #[test]
    fn keeps_a_valid_custom_format_while_another_format_is_selected() {
        let settings = parse("[display]\ndate_format = \"iso\"\ncustom_format = \"%Y\"\n").unwrap();
        assert_eq!(settings.date_format(), &DateFormat::Iso);
        assert_eq!(settings.custom_format(), Some("%Y"));

        let invalid = parse("[display]\ndate_format = \"iso\"\ncustom_format = \"%Q\"\n").unwrap();
        assert_eq!(invalid.custom_format(), None);
    }

    #[test]
    fn rejects_invalid_values_and_custom_directives() {
        for contents in [
            "[display]\ndate_format = \"wat\"\n",
            "[display]\ntime_format = \"13h\"\n",
            "[display]\ntimezone = \"mars\"\n",
            "[display]\ndate_format = \"custom\"\n",
            "[display]\ndate_format = \"custom\"\ncustom_format = \"%Q\"\n",
            "[display]\ndate_format = \"custom\"\ncustom_format = \"%F%n%T\"\n",
        ] {
            assert_eq!(
                parse(contents).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }
}
