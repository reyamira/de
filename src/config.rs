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
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            modified: true,
            date_format: DateFormat::Iso,
            time_format: TimeFormat::TwentyFourHour,
            timezone: Timezone::Local,
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

        Ok(Self {
            modified,
            date_format,
            time_format,
            timezone,
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
        if !document.contains_key("defaults") {
            document["defaults"] = TomlItem::Table(Table::new());
        }
        let table = document["defaults"]
            .as_table_mut()
            .ok_or_else(|| invalid_data("defaults must be a table"))?;
        table["hidden"] = value(self.show_hidden);
        table["sort"] = value(match self.sort_mode {
            SortMode::Name => "name",
            SortMode::Modified => "modified",
        });
        table["order"] = value(match self.sort_direction {
            SortDirection::Ascending => "ascending",
            SortDirection::Descending => "descending",
        });
        Ok(())
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
