use crate::{App, Entry};
use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const TWO_PANE_MIN_WIDTH: u16 = 58;

const MODIFIED_COLUMN_WIDTH: usize = 16;
const MODIFIED_COLUMN_GAP: usize = 2;
const MIN_NAME_COLUMN_WIDTH: usize = 12;
const ACCENT: Color = Color::LightCyan;
const TEXT: Color = Color::White;
const MUTED: Color = Color::Gray;

pub fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
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
            .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
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

    render_header(frame, app, header);
    if area.width >= TWO_PANE_MIN_WIDTH {
        render_two_panes(frame, app, body);
    } else {
        render_current_entries(frame, app, body, false);
    }
    render_footer(frame, app, footer);
}

fn render_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let path_width = area.width.saturating_sub(5) as usize;
    let path = shorten_left(&app.current_dir().to_string_lossy(), path_width);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " de ",
                Style::default()
                    .fg(Color::Black)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(path, Style::default().fg(TEXT).add_modifier(Modifier::BOLD)),
        ])),
        area,
    );
}

fn render_two_panes(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let [left, divider, right] = Layout::horizontal([
        Constraint::Percentage(44),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);

    render_current_entries(frame, app, left, true);
    let divider_lines = (0..divider.height)
        .map(|_| Line::styled("│", Style::default().fg(MUTED)))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(divider_lines), divider);
    render_preview(frame, app, right);
}

fn render_current_entries(frame: &mut Frame<'_>, app: &App, area: Rect, titled: bool) {
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

    if let Some(title) = title {
        frame.render_widget(
            Paragraph::new(pane_heading(" current", title.width as usize)).style(
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
            ),
            title,
        );
    }

    let mut lines = Vec::with_capacity(list.height as usize);
    if let Some(status) = app.status() {
        lines.push(Line::styled(
            shorten_right(status, list.width as usize),
            Style::default().fg(Color::LightRed),
        ));
    }

    let entry_rows = (list.height as usize).saturating_sub(lines.len());
    let offset = app.selected().saturating_add(1).saturating_sub(entry_rows);

    if app.entries().is_empty() && lines.len() < list.height as usize {
        lines.push(Line::styled("  (empty)", Style::default().fg(MUTED)));
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
        let text = entry_row(prefix, entry, list.width as usize);
        let style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            entry_style(entry)
        };
        lines.push(Line::styled(text, style));
    }
    frame.render_widget(Paragraph::new(lines), list);
}

fn render_preview(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let [title, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let heading = format!(" next · {}", app.preview().label());
    frame.render_widget(
        Paragraph::new(pane_heading(&heading, title.width as usize)).style(
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        ),
        title,
    );

    let mut lines = Vec::with_capacity(list.height as usize);
    if let Some(message) = app.preview().message() {
        lines.push(Line::styled(
            shorten_right(&format!("  {message}"), list.width as usize),
            Style::default().fg(MUTED),
        ));
    }
    for entry in app
        .preview()
        .entries()
        .iter()
        .take((list.height as usize).saturating_sub(lines.len()))
    {
        lines.push(Line::styled(
            entry_row("  ", entry, list.width as usize),
            entry_style(entry),
        ));
    }
    frame.render_widget(Paragraph::new(lines), list);
}

fn render_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if app.is_filtering() {
        let line = Line::from(vec![
            key(" / "),
            hint("filter: "),
            Span::styled(app.filter_query().to_owned(), Style::default().fg(TEXT)),
            Span::styled("▏", Style::default().fg(ACCENT)),
            hint(format!(
                "  {}/{} matches  esc clear",
                app.entries().len(),
                app.total_entry_count()
            )),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    let hidden = if app.show_hidden() { "on" } else { "off" };
    let sort = app.sort_mode().label();
    let direction = app.sort_direction().symbol();
    let line = if area.width >= 84 {
        Line::from(vec![
            key(" ↑↓"),
            hint(" select  "),
            key("pgup/dn"),
            hint(" jump  "),
            key("→"),
            hint(" open  "),
            key("←"),
            hint(" parent  "),
            key("/"),
            hint(" find  "),
            key("↵"),
            hint(" go  "),
            key("s"),
            hint(format!(" sort:{sort}{direction}  ")),
            key("."),
            hint(format!(" hidden:{hidden}")),
        ])
    } else if area.width >= 72 {
        Line::from(vec![
            key(" ↑↓"),
            hint(" select  "),
            key("pgup/dn"),
            hint(" jump  "),
            key("→"),
            hint(" open  "),
            key("←"),
            hint(" parent  "),
            key("/"),
            hint(" find  "),
            key("↵"),
            hint(" go  "),
            key("s"),
            hint(format!(" sort:{sort}{direction}")),
        ])
    } else if area.width >= 50 {
        Line::from(vec![
            key(" ↑↓"),
            hint("  "),
            key("pg↑↓"),
            hint("  "),
            key("→"),
            hint("open  "),
            key("←"),
            hint("back  "),
            key("/"),
            hint("find  "),
            key("s"),
            hint(format!(":{sort}{direction}")),
        ])
    } else if area.width >= 34 {
        Line::from(vec![
            key(" ↑↓"),
            hint("  "),
            key("pg↑↓"),
            hint("  "),
            key("/"),
            hint("find  "),
            key("s"),
            hint(format!(":{sort}{direction}  ")),
            key("↵"),
            hint("go"),
        ])
    } else {
        Line::from(vec![
            key(" ↑↓"),
            hint("  "),
            key("→"),
            hint("open  "),
            key("←"),
            hint("back  "),
            key("↵"),
            hint("go"),
        ])
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn key(value: impl Into<std::borrow::Cow<'static, str>>) -> Span<'static> {
    Span::styled(
        value,
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )
}

fn hint(value: impl Into<std::borrow::Cow<'static, str>>) -> Span<'static> {
    Span::styled(value, Style::default().fg(MUTED))
}

fn entry_style(entry: &Entry) -> Style {
    if entry.is_symlink {
        Style::default().fg(Color::LightMagenta)
    } else if entry.is_dir {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED)
    }
}

fn fill_row(prefix: &str, value: &str, width: usize) -> String {
    let name_width = width.saturating_sub(UnicodeWidthStr::width(prefix));
    let mut output = format!("{prefix}{}", shorten_right(value, name_width));
    let padding = width.saturating_sub(UnicodeWidthStr::width(output.as_str()));
    output.extend(std::iter::repeat_n(' ', padding));
    output
}

fn entry_row(prefix: &str, entry: &Entry, width: usize) -> String {
    if !shows_modified_column(width) {
        return fill_row(prefix, &entry.display_name(), width);
    }

    let name_column_width = width - MODIFIED_COLUMN_GAP - MODIFIED_COLUMN_WIDTH;
    let prefix_width = UnicodeWidthStr::width(prefix);
    let name_width = name_column_width.saturating_sub(prefix_width);
    let mut output = format!(
        "{prefix}{}",
        shorten_right(&entry.display_name(), name_width)
    );
    let padding = name_column_width.saturating_sub(UnicodeWidthStr::width(output.as_str()));
    output.extend(std::iter::repeat_n(' ', padding + MODIFIED_COLUMN_GAP));
    output.push_str(&format_modified(entry.modified));
    output
}

fn pane_heading(label: &str, width: usize) -> String {
    if !shows_modified_column(width) {
        return shorten_right(label, width);
    }

    let label_width = width - MODIFIED_COLUMN_GAP - MODIFIED_COLUMN_WIDTH;
    let mut output = shorten_right(label, label_width);
    let padding = label_width.saturating_sub(UnicodeWidthStr::width(output.as_str()));
    output.extend(std::iter::repeat_n(' ', padding + MODIFIED_COLUMN_GAP));
    output.push_str(&format!("{:>MODIFIED_COLUMN_WIDTH$}", "modified"));
    output
}

fn shows_modified_column(width: usize) -> bool {
    width >= MIN_NAME_COLUMN_WIDTH + MODIFIED_COLUMN_GAP + MODIFIED_COLUMN_WIDTH
}

fn format_modified(modified: Option<std::time::SystemTime>) -> String {
    modified.map_or_else(
        || format!("{:>MODIFIED_COLUMN_WIDTH$}", "—"),
        |modified| {
            DateTime::<Local>::from(modified)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        },
    )
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
    use tempfile::tempdir;

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
    fn modified_values_are_fixed_width_and_hidden_when_space_is_tight() {
        let value = format_modified(Some(std::time::SystemTime::UNIX_EPOCH));
        assert_eq!(
            UnicodeWidthStr::width(value.as_str()),
            MODIFIED_COLUMN_WIDTH
        );
        assert!(value.contains(':'));
        assert_eq!(
            UnicodeWidthStr::width(format_modified(None).as_str()),
            MODIFIED_COLUMN_WIDTH
        );
        assert!(!shows_modified_column(29));
        assert!(shows_modified_column(30));
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
}
