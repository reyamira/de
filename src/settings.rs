use crate::config::{
    date_format_name, sort_direction_name, sort_mode_name, time_format_name, timezone_name,
};
use crate::{App, DateFormat, DisplaySettings, TimeFormat, Timezone};

/// One row of the `de config` picker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Setting {
    Hidden,
    Sort,
    Order,
    Modified,
    DateFormat,
    TimeFormat,
    Timezone,
}

impl Setting {
    pub const ALL: [Self; 7] = [
        Self::Hidden,
        Self::Sort,
        Self::Order,
        Self::Modified,
        Self::DateFormat,
        Self::TimeFormat,
        Self::Timezone,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Hidden => "hidden",
            Self::Sort => "sort",
            Self::Order => "order",
            Self::Modified => "modified",
            Self::DateFormat => "date format",
            Self::TimeFormat => "time format",
            Self::Timezone => "timezone",
        }
    }
}

/// Which setting is highlighted, plus the date formats available to cycle
/// through. The values themselves live on the `App` so the preview pane
/// renders them exactly as the real picker would.
#[derive(Clone, Debug)]
pub struct SettingsPicker {
    selected: usize,
    date_formats: Vec<DateFormat>,
}

impl SettingsPicker {
    pub fn new(display: &DisplaySettings) -> Self {
        let mut date_formats = vec![
            DateFormat::Iso,
            DateFormat::Us,
            DateFormat::European,
            DateFormat::Relative,
        ];
        // A custom format can only be written in the editor, so it is offered
        // only when the file already has one.
        if let Some(format) = display.custom_format() {
            date_formats.push(DateFormat::Custom(format.to_owned()));
        }
        Self {
            selected: 0,
            date_formats,
        }
    }

    pub fn selected(&self) -> Setting {
        Setting::ALL[self.selected]
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        self.selected = (self.selected + 1).min(Setting::ALL.len() - 1);
    }

    /// Step the highlighted setting to its next (or previous) value, wrapping.
    pub fn change(&self, app: &mut App, forward: bool) {
        let mut display = app.display_settings().clone();
        match self.selected() {
            Setting::Hidden => return app.toggle_hidden(),
            Setting::Sort => return app.cycle_sort(),
            Setting::Order => return app.toggle_sort_direction(),
            Setting::Modified => display.set_shows_modified(!display.shows_modified()),
            Setting::DateFormat => {
                let count = self.date_formats.len();
                let current = self
                    .date_formats
                    .iter()
                    .position(|format| format == display.date_format())
                    .unwrap_or(0);
                let next = if forward {
                    (current + 1) % count
                } else {
                    (current + count - 1) % count
                };
                display.set_date_format(self.date_formats[next].clone());
            }
            Setting::TimeFormat => display.set_time_format(match display.time_format() {
                TimeFormat::TwelveHour => TimeFormat::TwentyFourHour,
                TimeFormat::TwentyFourHour => TimeFormat::TwelveHour,
            }),
            Setting::Timezone => display.set_timezone(match display.timezone() {
                Timezone::Local => Timezone::Utc,
                Timezone::Utc => Timezone::Local,
            }),
        }
        app.set_display_settings(display);
    }

    pub fn value(setting: Setting, app: &App) -> &'static str {
        let display = app.display_settings();
        match setting {
            Setting::Hidden | Setting::Modified => {
                let on = if setting == Setting::Hidden {
                    app.show_hidden()
                } else {
                    display.shows_modified()
                };
                if on { "on" } else { "off" }
            }
            Setting::Sort => sort_mode_name(app.sort_mode()),
            Setting::Order => sort_direction_name(app.sort_direction()),
            Setting::DateFormat => date_format_name(display.date_format()),
            Setting::TimeFormat => time_format_name(display.time_format()),
            Setting::Timezone => timezone_name(display.timezone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SortDirection, SortMode};
    use std::fs;
    use tempfile::tempdir;

    fn values(app: &App) -> Vec<&'static str> {
        Setting::ALL
            .iter()
            .map(|setting| SettingsPicker::value(*setting, app))
            .collect()
    }

    #[test]
    fn every_setting_changes_and_wraps_back() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join(".secret")).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let mut picker = SettingsPicker::new(app.display_settings());
        let initial = values(&app);
        assert_eq!(
            initial,
            ["off", "name", "ascending", "on", "iso", "24h", "local"]
        );

        for (index, setting) in Setting::ALL.into_iter().enumerate() {
            assert_eq!(picker.selected(), setting);
            picker.change(&mut app, true);
            assert_ne!(values(&app)[index], initial[index], "{setting:?}");
            let steps = if setting == Setting::DateFormat { 3 } else { 1 };
            for _ in 0..steps {
                picker.change(&mut app, true);
            }
            assert_eq!(values(&app)[index], initial[index], "{setting:?}");
            picker.move_down();
        }
        picker.move_down();
        assert_eq!(picker.selected(), Setting::Timezone);
    }

    #[test]
    fn changes_reach_the_listing_and_saved_values() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join(".secret")).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let mut picker = SettingsPicker::new(app.display_settings());

        picker.change(&mut app, true);
        assert!(app.entries().iter().any(|entry| entry.name == ".secret"));
        picker.move_down();
        picker.move_down();
        picker.change(&mut app, true);
        picker.move_down();
        picker.move_down();
        picker.change(&mut app, false);

        let defaults = app.browse_defaults();
        assert!(defaults.show_hidden);
        assert_eq!(defaults.sort_mode, SortMode::Name);
        assert_eq!(defaults.sort_direction, SortDirection::Descending);
        assert_eq!(app.display_settings().date_format(), &DateFormat::Relative);
    }

    #[test]
    fn custom_date_format_is_offered_only_when_configured() {
        let temp = tempdir().unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let picker = SettingsPicker::new(app.display_settings());
        assert!(
            !picker
                .date_formats
                .iter()
                .any(|format| matches!(format, DateFormat::Custom(_)))
        );

        let mut display = app.display_settings().clone();
        display.set_date_format(DateFormat::Custom("%Y".into()));
        app.set_display_settings(display);
        let mut picker = SettingsPicker::new(app.display_settings());
        for _ in 0..4 {
            picker.move_down();
        }
        picker.change(&mut app, true);
        assert_eq!(app.display_settings().date_format(), &DateFormat::Iso);
        picker.change(&mut app, false);
        assert_eq!(
            app.display_settings().date_format(),
            &DateFormat::Custom("%Y".into())
        );
    }
}
