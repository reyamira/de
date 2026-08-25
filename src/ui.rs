use crate::{App, DateFormat, DisplaySettings, Entry, Palette, TimeFormat, Timezone};
use chrono::{DateTime, Local, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use std::time::SystemTime;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const TWO_PANE_MIN_WIDTH: u16 = 58;

const MODIFIED_COLUMN_GAP: usize = 2;
const MIN_NAME_COLUMN_WIDTH: usize = 12;
pub fn render(frame: &mut Frame<'_>, app: &App) {
    render_picker(frame, app, false);
}

pub fn render_theme_preview(frame: &mut Frame<'_>, app: &App) {
    render_picker(frame, app, true);
}

fn render_picker(frame: &mut Frame<'_>, app: &App, theme_preview: bool) {
    let area = frame.area();
    let palette = app.theme().palette();
    let now = SystemTime::now();
    frame.render_widget(Clear, area);

    if area.height == 0 || area.width == 0 {
        return;
    }
    if area.height < 3 {
        frame.render_widget(
            Paragraph::new(shorten_left(
                &app.current_dir().to_string_lossy(),
                area.width as usize,
            ))
            .style(
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            area,
        );
        return;
    }

    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, app, header, &palette, theme_preview);
    if area.width >= TWO_PANE_MIN_WIDTH {
        render_two_panes(frame, app, body, &palette, now);
    } else {
        render_current_entries(frame, app, body, false, &palette, now);
    }
    if theme_preview {
        render_theme_footer(frame, app, footer, &palette);
    } else {
        render_footer(frame, app, footer, &palette);
    }
}

fn render_header(
    frame: &mut Frame<'_>,
    app: &App,
    area: Rect,
    palette: &Palette,
    theme_preview: bool,
) {
    let badge = if theme_preview { " de theme " } else { " de " };
    let badge_width = UnicodeWidthStr::width(badge);
    let path_width = (area.width as usize).saturating_sub(badge_width + 1);
    let path = shorten_left(&app.current_dir().to_string_lossy(), path_width);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(badge, emphasis_style(palette)),
            Span::raw(" "),
            Span::styled(
                path,
                Style::default()
                    .fg(palette.text)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        area,
    );
}

fn render_two_panes(
    frame: &mut Frame<'_>,
    app: &App,
    area: Rect,
    palette: &Palette,
    now: SystemTime,
) {
    let [left, divider, right] = Layout::horizontal([
        Constraint::Percentage(44),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);

    render_current_entries(frame, app, left, true, palette, now);
    let divider_lines = (0..divider.height)
        .map(|_| Line::styled("│", muted_style(palette)))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(divider_lines), divider);
    render_preview(frame, app, right, palette, now);
}

fn render_current_entries(
    frame: &mut Frame<'_>,
    app: &App,
    area: Rect,
    titled: bool,
    palette: &Palette,
    now: SystemTime,
) {
    if area.height == 0 {
        return;
    }
    let (title, list) = if titled {
        let [title, list] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
        (Some(title), list)
    } else {
        (None, area)
    };

    let modified_width = modified_column_width(app.entries(), app.display_settings(), now);
    if let Some(title) = title {
        frame.render_widget(
            Paragraph::new(pane_heading(
                " current",
                title.width as usize,
                modified_width,
            ))
            .style(
                Style::default()
                    .fg(palette.title)
                    .add_modifier(Modifier::BOLD),
            ),
            title,
        );
    }

    let mut lines = Vec::with_capacity(list.height as usize);
    if let Some(status) = app.status() {
        lines.push(Line::styled(
            shorten_right(status, list.width as usize),
            Style::default().fg(palette.error),
        ));
    }

    let entry_rows = (list.height as usize).saturating_sub(lines.len());
    let offset = app.selected().saturating_add(1).saturating_sub(entry_rows);

    if app.entries().is_empty() && lines.len() < list.height as usize {
        lines.push(Line::styled("  (empty)", muted_style(palette)));
    }

    for (index, entry) in app
        .entries()
        .iter()
        .enumerate()
        .skip(offset)
        .take(entry_rows)
    {
        let selected = index == app.selected();
        let prefix = if selected { "› " } else { "  " };
        let text = entry_row(
            prefix,
            entry,
            list.width as usize,
            modified_width,
            app.display_settings(),
            now,
        );
        let style = if selected {
            emphasis_style(palette)
        } else {
            entry_style(entry, palette)
        };
        lines.push(Line::styled(text, style));
    }
    frame.render_widget(Paragraph::new(lines), list);
}

fn render_preview(
    frame: &mut Frame<'_>,
    app: &App,
    area: Rect,
    palette: &Palette,
    now: SystemTime,
) {
    if area.height == 0 {
        return;
    }
    let [title, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let heading = format!(" next · {}", app.preview().label());
    let modified_width =
        modified_column_width(app.preview().entries(), app.display_settings(), now);
    frame.render_widget(
        Paragraph::new(pane_heading(&heading, title.width as usize, modified_width)).style(
            Style::default()
                .fg(palette.title)
                .add_modifier(Modifier::BOLD),
        ),
        title,
    );

    let mut lines = Vec::with_capacity(list.height as usize);
    if let Some(message) = app.preview().message() {
        lines.push(Line::styled(
            shorten_right(&format!("  {message}"), list.width as usize),
            muted_style(palette),
        ));
    }
    for entry in app
        .preview()
        .entries()
        .iter()
        .take((list.height as usize).saturating_sub(lines.len()))
    {
        lines.push(Line::styled(
            entry_row(
                "  ",
                entry,
                list.width as usize,
                modified_width,
                app.display_settings(),
                now,
            ),
            entry_style(entry, palette),
        ));
    }
    frame.render_widget(Paragraph::new(lines), list);
}

fn render_footer(frame: &mut Frame<'_>, app: &App, area: Rect, palette: &Palette) {
    if app.is_filtering() {
        let line = Line::from(vec![
            key(" / ", palette),
            hint("filter: ", palette),
            Span::styled(
                app.filter_query().to_owned(),
                Style::default().fg(palette.text),
            ),
            Span::styled("▏", Style::default().fg(palette.accent)),
            hint(
                format!(
                    "  {}/{} matches  esc clear",
                    app.entries().len(),
                    app.total_entry_count()
                ),
                palette,
            ),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    let hidden = if app.show_hidden() { "on" } else { "off" };
    let sort = app.sort_mode().label();
    let direction = app.sort_direction().symbol();
    let line = if area.width >= 84 {
        Line::from(vec![
            key(" ↑↓", palette),
            hint(" select  ", palette),
            key("pgup/dn", palette),
            hint(" jump  ", palette),
            key("→", palette),
            hint(" open  ", palette),
            key("←", palette),
            hint(" parent  ", palette),
            key("/", palette),
            hint(" find  ", palette),
            key("↵", palette),
            hint(" go  ", palette),
            key("s", palette),
            hint(format!(" sort:{sort}{direction}  "), palette),
            key(".", palette),
            hint(format!(" hidden:{hidden}"), palette),
        ])
    } else if area.width >= 72 {
        Line::from(vec![
            key(" ↑↓", palette),
            hint(" select  ", palette),
            key("pgup/dn", palette),
            hint(" jump  ", palette),
            key("→", palette),
            hint(" open  ", palette),
            key("←", palette),
            hint(" parent  ", palette),
            key("/", palette),
            hint(" find  ", palette),
            key("↵", palette),
            hint(" go  ", palette),
            key("s", palette),
            hint(format!(" sort:{sort}{direction}"), palette),
        ])
    } else if area.width >= 50 {
        Line::from(vec![
            key(" ↑↓", palette),
            hint("  ", palette),
            key("pg↑↓", palette),
            hint("  ", palette),
            key("→", palette),
            hint("open  ", palette),
            key("←", palette),
            hint("back  ", palette),
            key("/", palette),
            hint("find  ", palette),
            key("s", palette),
            hint(format!(":{sort}{direction}"), palette),
        ])
    } else if area.width >= 34 {
        Line::from(vec![
            key(" ↑↓", palette),
            hint("  ", palette),
            key("pg↑↓", palette),
            hint("  ", palette),
            key("/", palette),
            hint("find  ", palette),
            key("s", palette),
            hint(format!(":{sort}{direction}  "), palette),
            key("↵", palette),
            hint("go", palette),
        ])
    } else {
        Line::from(vec![
            key(" ↑↓", palette),
            hint("  ", palette),
            key("→", palette),
            hint("open  ", palette),
            key("←", palette),
            hint("back  ", palette),
            key("↵", palette),
            hint("go", palette),
        ])
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn render_theme_footer(frame: &mut Frame<'_>, app: &App, area: Rect, palette: &Palette) {
    let line = if area.width >= 48 {
        Line::from(vec![
            key(" ←→/↑↓", palette),
            hint(" preview  ", palette),
            Span::styled(format!(" {} ", app.theme().name()), emphasis_style(palette)),
            key("  enter", palette),
            hint(" save  ", palette),
            key("esc", palette),
            hint(" cancel", palette),
        ])
    } else {
        Line::from(vec![
            key(" ←→", palette),
            hint(" ", palette),
            Span::styled(app.theme().name().to_owned(), emphasis_style(palette)),
            key("  ↵", palette),
            hint("save  ", palette),
            key("esc", palette),
        ])
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn emphasis_style(palette: &Palette) -> Style {
    let style = Style::default()
        .fg(palette.emphasis_foreground)
        .bg(palette.emphasis_background)
        .add_modifier(Modifier::BOLD);
    if palette.reverse_emphasis {
        style.add_modifier(Modifier::REVERSED)
    } else {
        style
    }
}

fn muted_style(palette: &Palette) -> Style {
    let style = Style::default().fg(palette.muted);
    if palette.dim_muted {
        style.add_modifier(Modifier::DIM)
    } else {
        style
    }
}

fn key(value: impl Into<std::borrow::Cow<'static, str>>, palette: &Palette) -> Span<'static> {
    Span::styled(
        value,
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )
}

fn hint(value: impl Into<std::borrow::Cow<'static, str>>, palette: &Palette) -> Span<'static> {
    Span::styled(value, muted_style(palette))
}

fn entry_style(entry: &Entry, palette: &Palette) -> Style {
    if entry.is_symlink {
        Style::default().fg(palette.symlink)
    } else if entry.is_dir {
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        muted_style(palette)
    }
}

fn fill_row(prefix: &str, value: &str, width: usize) -> String {
    let name_width = width.saturating_sub(UnicodeWidthStr::width(prefix));
    let mut output = format!("{prefix}{}", shorten_right(value, name_width));
    let padding = width.saturating_sub(UnicodeWidthStr::width(output.as_str()));
    output.extend(std::iter::repeat_n(' ', padding));
    output
}

fn entry_row(
    prefix: &str,
    entry: &Entry,
    width: usize,
    modified_width: Option<usize>,
    display: &DisplaySettings,
    now: SystemTime,
) -> String {
    let Some(modified_width) = modified_width.filter(|value| shows_modified_column(width, *value))
    else {
        return fill_row(prefix, &entry.display_name(), width);
    };

    let name_column_width = width - MODIFIED_COLUMN_GAP - modified_width;
    let prefix_width = UnicodeWidthStr::width(prefix);
    let name_width = name_column_width.saturating_sub(prefix_width);
    let mut output = format!(
        "{prefix}{}",
        shorten_right(&entry.display_name(), name_width)
    );
    let padding = name_column_width.saturating_sub(UnicodeWidthStr::width(output.as_str()));
    output.extend(std::iter::repeat_n(' ', padding + MODIFIED_COLUMN_GAP));
    let modified = format_modified(entry.modified, display, now);
    let modified_padding = modified_width.saturating_sub(UnicodeWidthStr::width(modified.as_str()));
    output.extend(std::iter::repeat_n(' ', modified_padding));
    output.push_str(&modified);
    output
}

fn pane_heading(label: &str, width: usize, modified_width: Option<usize>) -> String {
    let Some(modified_width) = modified_width.filter(|value| shows_modified_column(width, *value))
    else {
        return shorten_right(label, width);
    };

    let label_width = width - MODIFIED_COLUMN_GAP - modified_width;
    let mut output = shorten_right(label, label_width);
    let padding = label_width.saturating_sub(UnicodeWidthStr::width(output.as_str()));
    output.extend(std::iter::repeat_n(' ', padding + MODIFIED_COLUMN_GAP));
    output.push_str(&format!("{:<modified_width$}", "modified"));
    output
}

fn modified_column_width(
    entries: &[Entry],
    display: &DisplaySettings,
    now: SystemTime,
) -> Option<usize> {
    if !display.shows_modified() {
        return None;
    }
    Some(
        entries
            .iter()
            .map(|entry| format_modified(entry.modified, display, now))
            .map(|value| UnicodeWidthStr::width(value.as_str()))
            .chain(std::iter::once(UnicodeWidthStr::width("modified")))
            .max()
            .expect("the modified heading always contributes a width"),
    )
}

fn shows_modified_column(width: usize, modified_width: usize) -> bool {
    width >= MIN_NAME_COLUMN_WIDTH + MODIFIED_COLUMN_GAP + modified_width
}

fn format_modified(
    modified: Option<SystemTime>,
    display: &DisplaySettings,
    now: SystemTime,
) -> String {
    let Some(modified) = modified else {
        return "—".into();
    };
    if display.date_format() == &DateFormat::Relative {
        return format_relative(modified, now);
    }

    let format = match display.date_format() {
        DateFormat::Iso => match display.time_format() {
            TimeFormat::TwelveHour => "%Y-%m-%d %I:%M %p",
            TimeFormat::TwentyFourHour => "%Y-%m-%d %H:%M",
        },
        DateFormat::Us => match display.time_format() {
            TimeFormat::TwelveHour => "%m/%d/%y %I:%M %p",
            TimeFormat::TwentyFourHour => "%m/%d/%y %H:%M",
        },
        DateFormat::European => match display.time_format() {
            TimeFormat::TwelveHour => "%d/%m/%y %I:%M %p",
            TimeFormat::TwentyFourHour => "%d/%m/%y %H:%M",
        },
        DateFormat::Custom(format) => format,
        DateFormat::Relative => unreachable!("relative timestamps return above"),
    };

    match display.timezone() {
        Timezone::Local => DateTime::<Local>::from(modified).format(format).to_string(),
        Timezone::Utc => DateTime::<Utc>::from(modified).format(format).to_string(),
    }
}

fn format_relative(modified: SystemTime, now: SystemTime) -> String {
    let (future, seconds) = match modified.duration_since(now) {
        Ok(duration) => (true, duration.as_secs()),
        Err(error) => (false, error.duration().as_secs()),
    };
    if seconds < 60 {
        return "just now".into();
    }

    let (amount, unit) = if seconds < 60 * 60 {
        (seconds / 60, "min")
    } else if seconds < 24 * 60 * 60 {
        (seconds / (60 * 60), "hr")
    } else if seconds < 30 * 24 * 60 * 60 {
        let days = seconds / (24 * 60 * 60);
        if days == 1 {
            return if future {
                "tomorrow".into()
            } else {
                "yesterday".into()
            };
        }
        (days, "day")
    } else if seconds < 365 * 24 * 60 * 60 {
        (seconds / (30 * 24 * 60 * 60), "month")
    } else {
        (seconds / (365 * 24 * 60 * 60), "year")
    };
    let plural = if amount == 1 || matches!(unit, "min" | "hr") {
        ""
    } else {
        "s"
    };
    if future {
        format!("in {amount} {unit}{plural}")
    } else {
        format!("{amount} {unit}{plural} ago")
    }
}

fn shorten_right(value: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(value) <= max_width {
        return value.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_owned();
    }

    let mut output = String::new();
    let mut width = 0;
    for character in value.chars() {
        let character_width = character.width().unwrap_or(0);
        if width + character_width >= max_width {
            break;
        }
        output.push(character);
        width += character_width;
    }
    output.push('…');
    output
}

fn shorten_left(value: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(value) <= max_width {
        return value.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_owned();
    }

    let wanted = max_width - 1;
    let mut suffix = String::new();
    let mut width = 0;
    for character in value.chars().rev() {
        let character_width = character.width().unwrap_or(0);
        if width + character_width > wanted {
            break;
        }
        suffix.insert(0, character);
        width += character_width;
    }
    format!("…{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::fs;
    use std::time::Duration;
    use tempfile::tempdir;
    use toml_edit::DocumentMut;

    fn display_settings(contents: &str) -> DisplaySettings {
        let document = contents.parse::<DocumentMut>().unwrap();
        DisplaySettings::from_document(&document).unwrap()
    }

    #[test]
    fn wide_render_shows_current_and_destination_panes() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("project")).unwrap();
        fs::write(temp.path().join("project/main.rs"), "fn main() {}").unwrap();
        fs::write(temp.path().join("README.md"), "hello").unwrap();
        let app = App::new(temp.path().to_path_buf()).unwrap();
        let backend = TestBackend::new(88, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("current"));
        assert!(rendered.contains("next · project/"));
        assert!(rendered.contains("modified"));
        assert!(rendered.contains("main.rs"));
        assert!(rendered.contains('│'));
        assert!(rendered.contains("hidden:off"));
        assert!(rendered.contains("sort:name↑"));
    }

    #[test]
    fn narrow_render_collapses_to_one_pane_without_wrapping() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("a-very-long-directory-name")).unwrap();
        let app = App::new(temp.path().to_path_buf()).unwrap();
        let backend = TestBackend::new(24, 5);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let rendered = terminal.backend().to_string();
        assert_eq!(terminal.backend().buffer().area.width, 24);
        assert!(rendered.contains("↵go"));
        assert!(!rendered.contains("next ·"));
        assert!(!rendered.contains("modified"));
    }

    #[test]
    fn filter_mode_renders_the_query_and_match_count() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("Cargo-project")).unwrap();
        fs::write(temp.path().join("Cargo.toml"), "[package]").unwrap();
        fs::write(temp.path().join("README.md"), "hello").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_filter();
        for character in "cargo".chars() {
            app.push_filter_char(character);
        }
        let backend = TestBackend::new(78, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("filter: cargo"));
        assert!(rendered.contains("2/3 matches"));
        assert!(!rendered.contains("README.md"));
    }

    #[test]
    fn modified_values_follow_display_preferences_and_available_space() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 24 * 60 * 60);
        let default = DisplaySettings::default();
        let value = format_modified(Some(SystemTime::UNIX_EPOCH), &default, now);
        assert_eq!(UnicodeWidthStr::width(value.as_str()), 16);
        assert!(value.contains(':'));
        assert_eq!(format_modified(None, &default, now), "—");
        assert!(!shows_modified_column(29, 16));
        assert!(shows_modified_column(30, 16));

        let relative = display_settings("[display]\ndate_format = \"relative\"\n");
        assert_eq!(
            format_modified(Some(now - Duration::from_secs(4 * 60)), &relative, now,),
            "4 min ago"
        );
        assert_eq!(
            format_modified(Some(now - Duration::from_secs(59)), &relative, now),
            "just now"
        );
        assert_eq!(
            format_modified(
                Some(now + Duration::from_secs(24 * 60 * 60)),
                &relative,
                now,
            ),
            "tomorrow"
        );

        let custom = display_settings(
            "[display]\ndate_format = \"custom\"\ncustom_format = \"%Y/%m/%d %H:%M UTC\"\ntimezone = \"utc\"\n",
        );
        assert_eq!(
            format_modified(Some(SystemTime::UNIX_EPOCH), &custom, now),
            "1970/01/01 00:00 UTC"
        );
    }

    #[test]
    fn modified_column_can_be_disabled() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("project")).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.set_display_settings(display_settings("[display]\nmodified = false\n"));
        let backend = TestBackend::new(88, 6);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert!(!terminal.backend().to_string().contains("modified"));
    }

    #[test]
    fn footer_reflects_the_active_sort_mode() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("project")).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.cycle_sort();
        app.toggle_sort_direction();
        let backend = TestBackend::new(88, 6);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert!(terminal.backend().to_string().contains("sort:time↓"));
    }

    #[test]
    fn theme_preview_uses_the_real_picker_and_selector_controls() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("project")).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let themes = crate::Config::built_ins();
        app.set_theme(themes.find("dark").unwrap().clone());
        let backend = TestBackend::new(88, 6);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_theme_preview(frame, &app))
            .unwrap();

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("de theme"));
        assert!(rendered.contains("next · project/"));
        assert!(rendered.contains("dark"));
        assert!(rendered.contains("preview"));
        assert!(rendered.contains("enter save"));
        assert!(rendered.contains("esc cancel"));
    }
}
