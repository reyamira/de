use crate::{
    App, DateFormat, DisplaySettings, Entry, Palette, Setting, SettingsPicker, TimeFormat, Timezone,
};
use chrono::{DateTime, Local, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use std::time::SystemTime;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const TWO_PANE_MIN_WIDTH: u16 = 58;
/// Header, settings heading, one row per setting, and footer.
pub const CONFIG_PICKER_MIN_HEIGHT: u16 = Setting::ALL.len() as u16 + 3;
const SETTINGS_PANE_WIDTH: u16 = 30;
const CONFIG_PREVIEW_MIN_WIDTH: u16 = 24;

const MODIFIED_COLUMN_GAP: usize = 2;
const MIN_NAME_COLUMN_WIDTH: usize = 12;
pub fn render(frame: &mut Frame<'_>, app: &App) {
    render_picker(frame, app, false);
}

pub fn render_theme_preview(frame: &mut Frame<'_>, app: &App) {
    if app.theme_sample() == crate::ThemeSample::Picker {
        let area = frame.area();
        frame.render_widget(Clear, area);
        render_editor_browser(frame, app, area, &app.theme().palette(), true);
    } else {
        render_picker(frame, app, true);
    }
}

/// Draw the active theme sample inside `area`, clipped to the frame.
///
/// For viewer samples, pass the same clipped rectangle to `viewer_body` and
/// the image renderer so their geometry matches this preview.
pub fn render_editor_preview(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let area = area.intersection(frame.area());
    if area.is_empty() {
        return;
    }
    let palette = app.theme().palette();
    frame.render_widget(Clear, area);
    if app.theme_sample() == crate::ThemeSample::Picker {
        render_editor_browser(frame, app, area, &palette, false);
        return;
    }

    render_viewer(frame, app, area, &palette);
    if area.height >= 3 {
        let [header, _, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(area);
        frame.render_widget(Clear, header);
        frame.render_widget(
            Paragraph::new(if app.is_markdown_source() {
                " Source"
            } else {
                " Rendered Markdown"
            })
            .style(
                Style::default()
                    .fg(palette.title)
                    .add_modifier(Modifier::BOLD),
            ),
            header,
        );
        frame.render_widget(Clear, footer);
        frame.render_widget(
            Paragraph::new(" Tab sample  PgUp/PgDown scroll").style(muted_style(&palette)),
            footer,
        );
    }
}

const BROWSER_SAMPLE_ROWS: usize = 9;

/// The last useful browser sample offset for a given body height.
pub fn browser_sample_scroll_max(rows: usize) -> usize {
    BROWSER_SAMPLE_ROWS.saturating_sub(rows)
}

/// Number of visible rows inside the browser sample's border.
pub fn browser_sample_page_rows(area: Rect) -> usize {
    let [_, outer, _] = browser_sample_areas(area);
    viewer_inner(outer).height as usize
}

fn browser_sample_areas(area: Rect) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(u16::from(area.height >= 2)),
    ])
    .areas(area)
}

fn render_editor_browser(
    frame: &mut Frame<'_>,
    app: &App,
    area: Rect,
    palette: &Palette,
    standalone: bool,
) {
    let [header, outer, footer] = browser_sample_areas(area);
    let body = viewer_inner(outer);
    if body != outer {
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_style(muted_style(palette)),
            outer,
        );
    }
    frame.render_widget(
        Paragraph::new(if standalone {
            format!(" Browser · {}", app.theme().name())
        } else {
            " Browser".to_owned()
        })
        .style(
            Style::default()
                .fg(palette.title)
                .add_modifier(Modifier::BOLD),
        ),
        header,
    );
    let examples = [
        ("Directory", true, false, false),
        ("File", false, false, false),
        ("Symlink", false, true, false),
        ("Selected directory", true, false, true),
        ("Selected file", false, false, true),
        ("Selected symlink", false, true, true),
    ];
    let entries: Vec<Entry> = examples
        .into_iter()
        .map(|(name, is_dir, is_symlink, _)| Entry {
            name: name.into(),
            path: name.into(),
            is_dir,
            is_symlink,
            modified: None,
        })
        .collect();
    let now = SystemTime::now();
    let mut lines = vec![
        Line::styled(" Normal interface text", Style::default().fg(palette.text)),
        Line::styled(" Muted information", muted_style(palette)),
        Line::styled(
            " Sample diagnostic (not an error)",
            Style::default().fg(palette.error),
        ),
    ];
    lines.extend(entries.iter().enumerate().map(|(index, entry)| {
        let selected = examples[index].3;
        Line::styled(
            entry_row(
                if selected { "› " } else { "  " },
                entry,
                body.width as usize,
                None,
                app.display_settings(),
                now,
            ),
            if selected {
                emphasis_style(palette)
            } else {
                entry_style(entry, palette)
            },
        )
    }));
    let offset = app
        .viewer_scroll()
        .min(browser_sample_scroll_max(body.height as usize));
    frame.render_widget(
        Paragraph::new(lines.into_iter().skip(offset).collect::<Vec<_>>()),
        body,
    );
    frame.render_widget(
        Paragraph::new(if standalone {
            " ←→ theme  Tab sample  PgUp/PgDown scroll  Enter save"
        } else {
            " Tab sample  PgUp/PgDown scroll"
        })
        .style(muted_style(palette)),
        footer,
    );
}

/// The `de config` picker: settings on the left, and the current directory
/// rendered with those settings on the right when there is room.
pub fn render_config_picker(frame: &mut Frame<'_>, app: &App, picker: &SettingsPicker) {
    let area = frame.area();
    let palette = app.theme().palette();
    frame.render_widget(Clear, area);
    if area.height < 3 || area.width == 0 {
        return;
    }

    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, app, header, &palette, " de config ");
    if area.width >= SETTINGS_PANE_WIDTH + 1 + CONFIG_PREVIEW_MIN_WIDTH {
        let [left, divider, right] = Layout::horizontal([
            Constraint::Length(SETTINGS_PANE_WIDTH),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(body);
        render_settings(frame, app, picker, left, &palette);
        let divider_lines = (0..divider.height)
            .map(|_| Line::styled("│", muted_style(&palette)))
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(divider_lines), divider);
        render_current_entries(frame, app, right, true, &palette, SystemTime::now());
    } else {
        render_settings(frame, app, picker, body, &palette);
    }
    render_config_footer(frame, footer, &palette);
}

fn render_settings(
    frame: &mut Frame<'_>,
    app: &App,
    picker: &SettingsPicker,
    area: Rect,
    palette: &Palette,
) {
    let width = area.width as usize;
    let mut lines = vec![Line::styled(
        fill_row("", " settings", width),
        Style::default()
            .fg(palette.title)
            .add_modifier(Modifier::BOLD),
    )];
    for setting in Setting::ALL {
        let value = SettingsPicker::value(setting, app);
        let line = if setting == picker.selected() {
            Line::styled(
                fill_row("› ", &format!("{:<12}‹ {value} ›", setting.label()), width),
                emphasis_style(palette),
            )
        } else {
            Line::from(vec![
                Span::styled(format!("  {:<12}", setting.label()), muted_style(palette)),
                Span::styled(format!("  {value}"), Style::default().fg(palette.text)),
            ])
        };
        lines.push(line);
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_config_footer(frame: &mut Frame<'_>, area: Rect, palette: &Palette) {
    let line = if area.width >= 48 {
        Line::from(vec![
            key(" ↑↓", palette),
            hint(" setting  ", palette),
            key("←→", palette),
            hint(" change  ", palette),
            key("enter", palette),
            hint(" save  ", palette),
            key("esc", palette),
            hint(" cancel", palette),
        ])
    } else {
        Line::from(vec![
            key(" ↑↓ ←→", palette),
            key("  ↵", palette),
            hint(" save  ", palette),
            key("esc", palette),
        ])
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn render_picker(frame: &mut Frame<'_>, app: &App, theme_preview: bool) {
    let area = frame.area();
    render_picker_in(frame, app, theme_preview, area);
}

fn render_picker_in(frame: &mut Frame<'_>, app: &App, theme_preview: bool, area: Rect) {
    let palette = app.theme().palette();
    let now = SystemTime::now();
    if app.editor().is_some() {
        let editor_area = file_editor_area(app, area);
        if editor_area != area {
            frame.render_widget(Clear, area);
            let [header, body, footer] = picker_areas(area);
            render_header(frame, app, header, &palette, " de ");
            let [left, divider, _] = picker_panes(body);
            render_current_entries(frame, app, left, true, &palette, now);
            frame.render_widget(
                Paragraph::new(
                    (0..divider.height)
                        .map(|_| Line::styled("│", muted_style(&palette)))
                        .collect::<Vec<_>>(),
                ),
                divider,
            );
            let editor = app.editor().unwrap();
            let status = if editor.confirming() {
                " Save changes? s Save · d Discard · c/Esc Cancel"
            } else {
                editor.message().unwrap_or(
                    " Editing preview · ^S save · Esc back · Shift+arrows select · ^Z undo",
                )
            };
            frame.render_widget(Paragraph::new(status).style(muted_style(&palette)), footer);
        }
        render_file_editor(frame, app, editor_area, &palette);
        return;
    }
    frame.render_widget(Clear, area);

    if app.is_viewing() {
        render_viewer(frame, app, area, &palette);
        return;
    }

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

    let [header, body, footer] = picker_areas(area);

    let badge = if theme_preview { " de theme " } else { " de " };
    render_header(frame, app, header, &palette, badge);
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

fn picker_areas(area: Rect) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area)
}

fn file_editor_area(app: &App, area: Rect) -> Rect {
    if !app.is_viewing() && area.width >= TWO_PANE_MIN_WIDTH && area.height >= 5 {
        picker_panes(picker_areas(area)[1])[2]
    } else {
        area
    }
}

fn file_editor_areas(app: &App, area: Rect) -> [Rect; 3] {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(u16::from(area.height >= 2)),
    ])
    .areas(area);
    let body = if app.is_viewing() {
        viewer_body(area).intersection(body)
    } else {
        Rect {
            width: body.width.saturating_sub(u16::from(body.width >= 2)),
            ..body
        }
    };
    [header, body, footer]
}

fn file_editor_track(app: &App, area: Rect, body: Rect) -> Option<Rect> {
    if app.is_viewing() {
        viewer_track(area)
    } else {
        (area.width >= 2 && !body.is_empty())
            .then(|| Rect::new(body.right(), body.y, 1, body.height))
    }
}

/// Movement uses the editor's real text body, including the preview-pane chrome.
pub fn editor_page_rows(app: &App, area: Rect) -> usize {
    file_editor_areas(app, file_editor_area(app, area))[1]
        .height
        .max(1) as usize
}

pub fn editor_body(app: &App, area: Rect) -> Rect {
    file_editor_areas(app, file_editor_area(app, area))[1]
}

pub fn resume_editor_cursor(app: &App) {
    if let Some(text) = app.editor_cache().borrow_mut().as_mut() {
        text.manual_scroll = false;
    }
}

pub fn set_editor_scroll(app: &App, scroll: usize, area: Rect) {
    if let Some(bar) = editor_scrollbar(app, area)
        && let Some(text) = app.editor_cache().borrow_mut().as_mut()
    {
        text.scroll = scroll.min(bar.maximum);
        text.manual_scroll = true;
    }
}

pub fn editor_scrollbar(app: &App, area: Rect) -> Option<ViewerScrollBar> {
    let editor = app.editor()?;
    let area = file_editor_area(app, area);
    let body = file_editor_areas(app, area)[1];
    if body.is_empty() {
        return None;
    }
    let track =
        file_editor_track(app, area, body).unwrap_or(Rect::new(body.x, body.y, 0, body.height));
    let gutter = source_gutter_width(editor.lines().len(), body.width);
    let mut cache = app.editor_cache().borrow_mut();
    let text = cache.get_or_insert_with(EditorText::new);
    text.update(
        editor,
        body.width.saturating_sub(gutter).max(1),
        &app.theme().palette(),
    );
    let maximum = text
        .lines
        .iter()
        .map(|line| line.rows.len())
        .sum::<usize>()
        .saturating_sub(body.height as usize);
    Some(scrollbar_geometry(
        track,
        text.scroll,
        maximum,
        body.height as usize,
    ))
}

fn render_header(frame: &mut Frame<'_>, app: &App, area: Rect, palette: &Palette, badge: &str) {
    if app.is_searching() {
        render_search_header(frame, app, area, palette);
        return;
    }
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

fn render_search_header(frame: &mut Frame<'_>, app: &App, area: Rect, palette: &Palette) {
    let mut spans = Vec::with_capacity(3);
    spans.push(Span::styled("/ ", muted_style(palette)));
    let query_width = (area.width as usize).saturating_sub(3);
    let query = app.search_query();
    let window = search_window(query, query.len()..query.len(), query_width);
    let mut text = String::new();
    if window.before {
        text.push('…');
    }
    text.push_str(&query[window.bytes]);
    if window.after {
        text.push('…');
    }
    spans.push(Span::styled(text, Style::default().fg(palette.text)));
    spans.push(Span::styled("▏", Style::default().fg(palette.accent)));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_two_panes(
    frame: &mut Frame<'_>,
    app: &App,
    area: Rect,
    palette: &Palette,
    now: SystemTime,
) {
    let [left, divider, right] = picker_panes(area);

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

    let modified_width = if app.is_searching() {
        None
    } else {
        modified_column_width(app.entries(), app.display_settings(), now)
    };
    if let Some(title) = title {
        let heading = if app.is_searching() {
            format!(" {}", app.current_dir().display())
        } else {
            " current".to_owned()
        };
        frame.render_widget(
            Paragraph::new(pane_heading(&heading, title.width as usize, modified_width)).style(
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
        lines.push(Line::styled(
            if app.is_searching() {
                if app.search_pending() {
                    "  Searching…"
                } else {
                    "  No matches"
                }
            } else {
                "  (empty)"
            },
            muted_style(palette),
        ));
    }

    let matcher = crate::matching::LiteralMatcher::new(app.search_query());
    for (index, entry) in app
        .entries()
        .iter()
        .enumerate()
        .skip(offset)
        .take(entry_rows)
    {
        let selected = index == app.selected();
        let prefix = if selected { "› " } else { "  " };
        if let Some(hit) = app.search_hit(index) {
            lines.push(search_result_line(
                &entry.display_name(),
                hit,
                app.search_file_line_count(index),
                list.width as usize,
                selected,
                palette,
                &matcher,
            ));
        } else {
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

    if app.preview().document().is_some() || app.preview().message().is_some() {
        let cached = viewer_text(app, list.width, palette);
        let preview_matches = &cached.browser_matches;
        let offset = app
            .search_hit(app.selected())
            .and_then(|hit| {
                let line = hit.line?;
                let start = app.preview().document().and_then(|doc| {
                    crate::document::logical_lines(&doc.source)
                        .nth(line - 1)
                        .map(|text| text.as_ptr() as usize - doc.source.as_ptr() as usize)
                })?;
                let byte = start + hit.first_match.start;
                Some(
                    cached
                        .geometry
                        .iter()
                        .position(|cells| cells.iter().any(|cell| cell.bytes.contains(&byte)))
                        .unwrap_or(0),
                )
            })
            .unwrap_or(0);
        let lines: Vec<_> = (offset..cached.rows.len())
            .take(list.height as usize)
            .map(|row| highlighted_source_row(app, &cached, row, palette, preview_matches))
            .collect();
        frame.render_widget(Paragraph::new(lines), list);
        return;
    }
    let lines = app
        .preview()
        .entries()
        .iter()
        .take(list.height as usize)
        .map(|entry| {
            Line::styled(
                entry_row(
                    "  ",
                    entry,
                    list.width as usize,
                    modified_width,
                    app.display_settings(),
                    now,
                ),
                entry_style(entry, palette),
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), list);
}

pub(crate) fn picker_panes(area: Rect) -> [Rect; 3] {
    Layout::horizontal([
        Constraint::Percentage(44),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area)
}

fn viewer_outer(area: Rect) -> Rect {
    if area.height < 3 {
        area
    } else {
        let [_, body, _] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(area);
        body
    }
}

pub fn viewer_body(area: Rect) -> Rect {
    let mut inner = viewer_inner(viewer_outer(area));
    if inner.width >= 2 && inner.height > 0 {
        inner.width -= 1;
    }
    inner
}

fn viewer_track(area: Rect) -> Option<Rect> {
    let inner = viewer_inner(viewer_outer(area));
    (inner.width >= 2 && inner.height > 0)
        .then(|| Rect::new(inner.x + inner.width - 1, inner.y, 1, inner.height))
}

/// Scrollbar geometry and scroll units for the current viewer mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerScrollBar {
    pub track: Rect,
    pub thumb: Rect,
    pub offset: usize,
    pub maximum: usize,
    pub viewport: usize,
}

impl ViewerScrollBar {
    /// Map an inline-local row and the grabbed point within the thumb to a scroll offset.
    pub fn scroll_at(&self, terminal_local_row: u16, grab_offset: u16) -> usize {
        let travel = self.track.height.saturating_sub(self.thumb.height);
        if travel == 0 || self.maximum == 0 {
            return 0;
        }
        let position = terminal_local_row
            .saturating_sub(self.track.y)
            .saturating_sub(grab_offset)
            .min(travel);
        ((position as u128 * self.maximum as u128) / travel as u128) as usize
    }
}

fn scrollbar_geometry(
    track: Rect,
    offset: usize,
    maximum: usize,
    viewport: usize,
) -> ViewerScrollBar {
    let offset = offset.min(maximum);
    let length = if maximum == 0 {
        track.height
    } else {
        ((track.height as u128 * viewport as u128) / (viewport as u128 + maximum as u128)).max(1)
            as u16
    };
    let travel = track.height - length;
    let top = if maximum == 0 {
        0
    } else {
        ((travel as u128 * offset as u128) / maximum as u128) as u16
    };
    ViewerScrollBar {
        track,
        thumb: Rect::new(track.x, track.y + top, 1, length),
        offset,
        maximum,
        viewport,
    }
}

/// Return the interactive scrollbar for an open viewer with a known scroll range.
pub fn viewer_scrollbar(app: &App, area: Rect) -> Option<ViewerScrollBar> {
    if !app.is_viewing() {
        return None;
    }
    let track = viewer_track(area)?;
    let maximum = viewer_scroll_max(app, area);
    if maximum == usize::MAX {
        return None;
    }
    let rows = viewer_body(area).height as usize;
    let viewport = if app.is_rendered_markdown_view() {
        rows.saturating_mul(app.markdown_scroll_step())
    } else {
        rows
    };
    Some(scrollbar_geometry(
        track,
        app.viewer_scroll(),
        maximum,
        viewport,
    ))
}

fn viewer_inner(outer: Rect) -> Rect {
    if outer.width >= 3 && outer.height >= 3 {
        Rect::new(outer.x + 1, outer.y + 1, outer.width - 2, outer.height - 2)
    } else {
        outer
    }
}

/// Number of text rows available to a full-width viewer.
pub fn viewer_page_rows(area: Rect) -> usize {
    viewer_body(area).height as usize
}

/// The image rectangle is derived from the same layout used by the renderer.
pub fn preview_image_area(app: &App, area: Rect) -> Option<Rect> {
    if app.is_rendered_markdown_view() {
        app.markdown_image()?;
        let body = viewer_body(area);
        return (body.width > 0 && body.height > 0).then_some(body);
    }
    app.preview().image()?;
    let image_area = if app.is_viewing() {
        viewer_body(area)
    } else {
        if area.height < 3 || area.width < TWO_PANE_MIN_WIDTH {
            return None;
        }
        let [_, body, _] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(area);
        let [_, _, right] = picker_panes(body);
        let [_, content] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(right);
        content
    };
    (image_area.width > 0 && image_area.height > 0).then_some(image_area)
}

fn preview_text_lines(
    app: &App,
    width: u16,
    palette: &Palette,
    offset: usize,
    limit: usize,
    count_all: bool,
) -> (Vec<Line<'static>>, usize) {
    if width == 0 {
        return (Vec::new(), 0);
    }
    let mut notices = Vec::new();
    if let Some(message) = app.preview().message() {
        notices.push(Line::styled(message.to_owned(), muted_style(palette)));
    }
    if let Some(notice) = app
        .preview()
        .document()
        .and_then(|document| document.notice.as_ref())
    {
        notices.push(Line::styled(notice.clone(), muted_style(palette)));
    }
    let document_lines = app
        .preview()
        .document()
        .map(|document| document.lines.as_slice())
        .unwrap_or(&[]);
    if !app.is_reading()
        && app
            .preview()
            .document()
            .is_some_and(|document| document.source_lines)
    {
        let (rows, _) = numbered_source_rows(&notices, document_lines, width, palette);
        let count = rows.len();
        return (rows.into_iter().skip(offset).take(limit).collect(), count);
    }
    if app.is_reading() {
        let paragraphs = reading_lines(document_lines);
        wrap_text_lines(
            notices.iter().chain(paragraphs.iter()),
            width,
            offset,
            limit,
            count_all,
        )
    } else {
        wrap_text_lines(
            notices.iter().chain(document_lines.iter()),
            width,
            offset,
            limit,
            count_all,
        )
    }
}

// Indented lines and blank lines remain literal. Only unindented prose joins.
fn reading_lines(lines: &[Line<'static>]) -> Vec<Line<'static>> {
    let mut result: Vec<Line<'static>> = Vec::new();
    let mut joining = false;
    for line in lines {
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        let prose =
            !text.trim().is_empty() && !text.chars().next().is_some_and(char::is_whitespace);
        if prose && joining {
            let previous = result.last_mut().unwrap();
            while let Some(span) = previous.spans.last_mut() {
                let trimmed = span.content.trim_end().len();
                span.content.to_mut().truncate(trimmed);
                if !span.content.is_empty() {
                    break;
                }
                previous.spans.pop();
            }
            previous.spans.push(Span::raw(" "));
            previous.spans.extend(line.spans.iter().cloned());
        } else {
            result.push(line.clone());
        }
        joining = prose;
    }
    result
}

// Keep only the current word and whitespace in memory, and stop after the
// visible rows unless the caller needs a complete count.
fn wrap_text_lines<'a>(
    lines: impl Iterator<Item = &'a Line<'static>>,
    width: u16,
    offset: usize,
    limit: usize,
    count_all: bool,
) -> (Vec<Line<'static>>, usize) {
    if width == 0 {
        return (Vec::new(), 0);
    }
    let mut rows = WrappedRows {
        width: width as usize,
        offset,
        end: offset.saturating_add(limit),
        count_all,
        count: 0,
        used: 0,
        spans: Vec::new(),
        result: Vec::new(),
    };
    for line in lines {
        let mut word = Vec::new();
        let mut word_width = 0;
        let mut spaces = Vec::new();
        let mut space_width = 0;
        let mut has_word = false;
        for grapheme in line.styled_graphemes(line.style) {
            let cells = UnicodeWidthStr::width(grapheme.symbol);
            if cells > rows.width {
                continue;
            }
            if grapheme.symbol.chars().all(char::is_whitespace) {
                if !word.is_empty() {
                    rows.word(&mut word, word_width, &mut spaces, space_width, has_word);
                    word_width = 0;
                    space_width = 0;
                    has_word = true;
                }
                if rows.finished() {
                    return (rows.result, rows.count);
                }
                if space_width + cells > rows.width {
                    if has_word {
                        continue;
                    }
                    rows.append(&mut spaces);
                    space_width = 0;
                }
                spaces.push(grapheme);
                space_width += cells;
            } else {
                if word_width + cells > rows.width {
                    rows.word(&mut word, word_width, &mut spaces, space_width, has_word);
                    word_width = 0;
                    space_width = 0;
                    has_word = true;
                    if rows.finished() {
                        return (rows.result, rows.count);
                    }
                }
                word.push(grapheme);
                word_width += cells;
            }
        }
        if !word.is_empty() {
            rows.word(&mut word, word_width, &mut spaces, space_width, has_word);
        }
        if has_word {
            let mut remaining = rows.width.saturating_sub(rows.used);
            let fitting = spaces
                .iter()
                .take_while(|grapheme| {
                    let cells = UnicodeWidthStr::width(grapheme.symbol);
                    if cells > remaining {
                        false
                    } else {
                        remaining -= cells;
                        true
                    }
                })
                .count();
            spaces.truncate(fitting);
        }
        rows.append(&mut spaces);
        rows.emit();
        if rows.finished() {
            break;
        }
    }
    (rows.result, rows.count)
}

struct WrappedRows {
    width: usize,
    offset: usize,
    end: usize,
    count_all: bool,
    count: usize,
    used: usize,
    spans: Vec<Span<'static>>,
    result: Vec<Line<'static>>,
}

impl WrappedRows {
    fn finished(&self) -> bool {
        !self.count_all && self.count >= self.end
    }

    fn emit(&mut self) {
        if self.count >= self.offset && self.count < self.end {
            self.result
                .push(Line::from(std::mem::take(&mut self.spans)));
        } else {
            self.spans.clear();
        }
        self.count += 1;
        self.used = 0;
    }

    fn append(&mut self, graphemes: &mut Vec<ratatui::text::StyledGrapheme<'_>>) {
        for grapheme in graphemes.drain(..) {
            let cells = UnicodeWidthStr::width(grapheme.symbol);
            if self.used > 0 && self.used + cells > self.width {
                self.emit();
            }
            if self.count >= self.offset && self.count < self.end {
                if let Some(last) = self
                    .spans
                    .last_mut()
                    .filter(|last| last.style == grapheme.style)
                {
                    last.content.to_mut().push_str(grapheme.symbol);
                } else {
                    self.spans
                        .push(Span::styled(grapheme.symbol.to_owned(), grapheme.style));
                }
            }
            self.used += cells;
        }
    }

    fn word(
        &mut self,
        word: &mut Vec<ratatui::text::StyledGrapheme<'_>>,
        word_width: usize,
        spaces: &mut Vec<ratatui::text::StyledGrapheme<'_>>,
        space_width: usize,
        has_word: bool,
    ) {
        if has_word && self.used > 0 && self.used + space_width + word_width > self.width {
            self.emit();
            spaces.clear();
        } else {
            self.append(spaces);
        }
        self.append(word);
    }
}

#[derive(Debug)]
pub(crate) struct ViewerText {
    width: u16,
    reading: bool,
    browser_query: String,
    browser_matches: Vec<std::ops::Range<usize>>,
    rows: Vec<Line<'static>>,
    source_rows: Vec<usize>,
    geometry: Vec<Vec<SourceCell>>,
}

#[derive(Debug, Clone, Default)]
struct SourceCell {
    bytes: std::ops::Range<usize>,
    text: String,
    style: Style,
    width: usize,
}

fn source_geometry(
    document: &crate::document::Document,
    width: u16,
    reading: bool,
    notices: &[Line<'static>],
    palette: &Palette,
) -> (Vec<Line<'static>>, Vec<usize>, Vec<Vec<SourceCell>>) {
    use unicode_segmentation::UnicodeSegmentation;
    let gutter = if reading {
        0
    } else {
        source_gutter_width(document.lines.len(), width)
    };
    let capacity = width.saturating_sub(gutter).max(1) as usize;
    let (mut rows, _) = wrap_text_lines(notices.iter(), width, 0, usize::MAX, true);
    let mut sources = vec![0; rows.len()];
    let mut geometry = vec![Vec::new(); rows.len()];
    let mut paragraphs: Vec<(usize, Vec<SourceCell>)> = Vec::new();
    let mut joining = false;
    for (index, source) in crate::document::logical_lines(&document.source)
        .take(document.lines.len())
        .enumerate()
    {
        let offset = source.as_ptr() as usize - document.source.as_ptr() as usize;
        let styles = &document.lines[index];
        let mut display_byte = 0;
        let mut cells = Vec::new();
        for (byte, grapheme) in source.grapheme_indices(true) {
            let mut text = String::with_capacity(grapheme.len());
            for ch in grapheme.chars() {
                match ch {
                    '\t' => text.push_str("    "),
                    ch if ch.is_control() => text.push('\u{fffd}'),
                    ch => text.push(ch),
                }
            }
            let mut span_end = 0;
            let style = styles
                .spans
                .iter()
                .find_map(|span| {
                    span_end += span.content.len();
                    (display_byte < span_end).then_some(styles.style.patch(span.style))
                })
                .unwrap_or_else(|| Style::default().fg(palette.text));
            display_byte += text.len();
            let cells_width = UnicodeWidthStr::width(text.as_str()).min(capacity);
            cells.push(SourceCell {
                bytes: offset + byte..offset + byte + grapheme.len(),
                text,
                style,
                width: cells_width,
            });
        }
        let prose = reading
            && !source.trim().is_empty()
            && !source.chars().next().is_some_and(char::is_whitespace);
        if prose && joining {
            let previous = &mut paragraphs.last_mut().unwrap().1;
            while previous
                .last()
                .is_some_and(|cell| cell.text.chars().all(char::is_whitespace))
            {
                previous.pop();
            }
            let start = previous.last().map_or(offset, |cell| cell.bytes.end);
            previous.push(SourceCell {
                bytes: start..offset,
                text: " ".into(),
                style: Style::default().fg(palette.text),
                width: 1,
            });
            previous.extend(cells);
        } else {
            paragraphs.push((index + 1, cells));
        }
        joining = prose;
    }
    for (line, mut cells) in paragraphs {
        let mut wrapped: Vec<Vec<SourceCell>> = Vec::new();
        let mut row = Vec::new();
        let mut used = 0;
        let mut position = 0;
        let mut has_word = false;
        while position < cells.len() {
            let whitespace = cells[position].text.chars().all(char::is_whitespace);
            let end = position
                + cells[position..]
                    .iter()
                    .take_while(|cell| cell.text.chars().all(char::is_whitespace) == whitespace)
                    .count();
            let word_width = cells[position..end]
                .iter()
                .map(|cell| cell.width)
                .sum::<usize>();
            if !whitespace && has_word && used > 0 && used + word_width > capacity {
                wrapped.push(std::mem::take(&mut row));
                used = 0;
            }
            for cell in &mut cells[position..end] {
                if used > 0 && used + cell.width > capacity {
                    wrapped.push(std::mem::take(&mut row));
                    used = 0;
                }
                used += cell.width;
                row.push(std::mem::take(cell));
            }
            has_word |= !whitespace;
            position = end;
        }
        wrapped.push(row);
        for (continuation, cells) in wrapped.into_iter().enumerate() {
            let mut spans = Vec::new();
            if gutter > 0 {
                let label = if continuation == 0 {
                    format!("{:>digits$} │", line, digits = gutter as usize - 2)
                } else {
                    " ".repeat(gutter as usize)
                };
                spans.push(Span::styled(label, muted_style(palette)));
            }
            spans.extend(
                cells
                    .iter()
                    .map(|cell| Span::styled(cell.text.clone(), cell.style)),
            );
            rows.push(Line::from(spans));
            sources.push(line);
            geometry.push(cells);
        }
    }
    (rows, sources, geometry)
}

pub(crate) fn source_hit_test(app: &App, width: u16, column: u16, row: usize) -> Option<usize> {
    let cached = viewer_text(app, width, &app.theme().palette());
    let cells = cached.geometry.get(row)?;
    if cells.is_empty() {
        let line = *cached.source_rows.get(row)?;
        let source = &app.preview().document()?.source;
        return crate::document::logical_lines(source)
            .nth(line.checked_sub(1)?)
            .map(|text| text.as_ptr() as usize - source.as_ptr() as usize);
    }
    let gutter = if app.is_reading() {
        0
    } else {
        source_gutter_width(app.preview().document()?.lines.len(), width)
    };
    let mut x = column.saturating_sub(gutter) as usize;
    for cell in cells {
        if x.saturating_mul(2) < cell.width {
            return Some(cell.bytes.start);
        }
        if x < cell.width {
            return Some(cell.bytes.end);
        }
        x = x.saturating_sub(cell.width);
    }
    cells.last().map(|cell| cell.bytes.end)
}

pub(crate) fn source_byte_offset(app: &App, width: u16, byte: usize) -> usize {
    let cached = viewer_text(app, width, &app.theme().palette());
    cached
        .geometry
        .iter()
        .position(|cells| cells.iter().any(|cell| cell.bytes.contains(&byte)))
        .unwrap_or(0)
}

fn highlighted_source_row(
    app: &App,
    cached: &ViewerText,
    row: usize,
    palette: &Palette,
    matches: &[std::ops::Range<usize>],
) -> Line<'static> {
    let Some(cells) = cached.geometry.get(row).filter(|cells| !cells.is_empty()) else {
        return cached.rows[row].clone();
    };
    let mut spans = Vec::new();
    let gutter = if app.is_reading() {
        0
    } else {
        source_gutter_width(app.preview().document().unwrap().lines.len(), cached.width)
    };
    if gutter > 0 {
        spans.push(cached.rows[row].spans[0].clone());
    }
    let selection = app.selection_range();
    let active = app.active_match();
    let overlaps =
        |a: &std::ops::Range<usize>, b: &std::ops::Range<usize>| a.start < b.end && b.start < a.end;
    for cell in cells {
        let style = if selection
            .as_ref()
            .is_some_and(|range| overlaps(range, &cell.bytes))
        {
            emphasis_style(palette)
        } else if active
            .as_ref()
            .is_some_and(|range| overlaps(range, &cell.bytes))
        {
            emphasis_style(palette).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else if matches
            .get(matches.partition_point(|range| range.end <= cell.bytes.start))
            .is_some_and(|range| range.start < cell.bytes.end)
        {
            cell.style
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            cell.style
        };
        spans.push(Span::styled(cell.text.clone(), style));
    }
    Line::from(spans)
}

fn search_result_line(
    name: &str,
    hit: &crate::search::SearchHit,
    line_count: usize,
    width: usize,
    selected: bool,
    palette: &Palette,
    matcher: &crate::matching::LiteralMatcher,
) -> Line<'static> {
    use crate::search::SearchHitKind;
    let prefix = if selected { "› " } else { "  " };
    let base = if selected {
        emphasis_style(palette)
    } else {
        Style::default().fg(palette.text)
    };
    let muted = if selected {
        base
    } else {
        base.patch(muted_style(palette))
    };
    let matched = if selected {
        base.add_modifier(Modifier::UNDERLINED)
    } else {
        base.fg(palette.accent)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    };
    let mut spans = vec![Span::styled(prefix.to_owned(), base)];
    let available = width.saturating_sub(UnicodeWidthStr::width(prefix));
    if hit.kind == SearchHitKind::Line {
        let gutter = format!("  {:>5} ", hit.line.unwrap_or(1));
        let gutter_width = gutter.len();
        if available > gutter_width + 2 {
            spans.push(Span::styled(gutter, muted));
        }
        let text_width = if available > gutter_width + 2 {
            available - gutter_width
        } else {
            available
        };
        let first = hit.first_match.start.saturating_sub(hit.excerpt_start)
            ..hit.first_match.end.saturating_sub(hit.excerpt_start);
        append_search_window(
            &mut spans,
            &hit.excerpt,
            first,
            text_width,
            base,
            Some(matcher),
            matched,
        );
    } else {
        let undecorated = if hit.is_symlink {
            name.strip_suffix('@').unwrap_or(name)
        } else {
            name
        };
        let trimmed = if hit.is_dir {
            undecorated.trim_end_matches('/')
        } else {
            undecorated
        };
        let basename_start = trimmed.rfind('/').map_or(0, |byte| byte + 1);
        let (directory, basename) = name.split_at(basename_start);
        let count = if line_count == 1 {
            "  1 line".to_owned()
        } else if line_count > 0 {
            format!("  {line_count} lines")
        } else {
            String::new()
        };
        let count_width = count.len();
        let basename_width = UnicodeWidthStr::width(basename);
        let show_count = line_count > 0 && available >= basename_width + count_width + 4;
        let path_width = available.saturating_sub(if show_count { count_width } else { 0 });
        let header_style = if hit.is_dir && !selected {
            base.fg(palette.accent).add_modifier(Modifier::BOLD)
        } else {
            base
        };
        let highlight = (hit.kind == SearchHitKind::Name).then_some(matcher);
        let directory_match =
            hit.kind == SearchHitKind::Name && hit.first_match.start < basename_start;
        let directory_reserve = if directory_match {
            let match_end = hit.first_match.end.min(directory.len());
            let match_width = UnicodeWidthStr::width(&directory[hit.first_match.start..match_end]);
            (match_width + 3)
                .min((path_width / 2).max(4))
                .min(path_width)
        } else {
            0
        };
        if basename_width >= path_width && !directory_match {
            let first = if hit.kind == SearchHitKind::Name {
                hit.first_match.start.saturating_sub(basename_start)
                    ..hit.first_match.end.saturating_sub(basename_start)
            } else {
                0..0
            };
            append_search_window(
                &mut spans,
                basename,
                first,
                path_width,
                header_style,
                highlight,
                matched,
            );
        } else {
            // Keep a directory match visible even when the basename also needs clipping.
            let directory_width = path_width
                .saturating_sub(basename_width)
                .max(directory_reserve);
            let basename_budget = path_width.saturating_sub(directory_width);
            let first = if directory_match {
                hit.first_match.clone()
            } else {
                directory.len()..directory.len()
            };
            if !directory.is_empty() {
                append_search_window(
                    &mut spans,
                    directory.trim_end_matches('/'),
                    first,
                    directory_width.saturating_sub(1),
                    muted,
                    highlight,
                    matched,
                );
                let slash_style = if highlight.is_some()
                    && hit.first_match.start < basename_start
                    && hit.first_match.end >= basename_start
                {
                    matched
                } else {
                    muted
                };
                spans.push(Span::styled("/", slash_style));
            }
            let first = hit.first_match.start.saturating_sub(basename_start)
                ..hit.first_match.end.saturating_sub(basename_start);
            append_search_window(
                &mut spans,
                basename,
                first,
                basename_budget,
                header_style,
                highlight,
                matched,
            );
        }
        if show_count {
            let used = spans.iter().map(Span::width).sum::<usize>();
            let padding = width.saturating_sub(used + count_width);
            if padding > 0 {
                spans.push(Span::styled(" ".repeat(padding), muted));
            }
            spans.push(Span::styled(count, muted));
        }
    }
    let used = spans.iter().map(Span::width).sum::<usize>();
    if selected && used < width {
        spans.push(Span::styled(" ".repeat(width - used), base));
    }
    Line::from(spans)
}

struct SearchWindow {
    bytes: std::ops::Range<usize>,
    before: bool,
    after: bool,
}

/// Keep complete graphemes and reserve the first match before context or markers.
fn search_window(source: &str, first: std::ops::Range<usize>, width: usize) -> SearchWindow {
    use unicode_segmentation::UnicodeSegmentation;
    let mut window = SearchWindow {
        bytes: 0..0,
        before: false,
        after: false,
    };
    if width == 0 {
        return window;
    }
    if UnicodeWidthStr::width(source) <= width {
        window.bytes.end = source.len();
        return window;
    }
    let target = first.start.min(source.len());
    let (target_start, target_text) = source
        .grapheme_indices(true)
        .take_while(|(byte, _)| *byte <= target)
        .last()
        .unwrap_or((0, ""));
    let target_width = UnicodeWidthStr::width(target_text);
    window.before = target_start > 0 && width > target_width;
    window.after = target_start + target_text.len() < source.len()
        && width > target_width + usize::from(window.before);
    let available = width.saturating_sub(usize::from(window.before) + usize::from(window.after));
    let context_width = if first.start >= source.len() {
        available.saturating_sub(target_width)
    } else {
        (width / 4).min(available.saturating_sub(target_width))
    };
    let mut context = 0;
    let mut start = target_start;
    for (byte, grapheme) in source[..start].grapheme_indices(true).rev() {
        let cells = UnicodeWidthStr::width(grapheme);
        if context + cells > context_width {
            break;
        }
        context += cells;
        start = byte;
    }
    window.before &= start > 0;
    let available = width.saturating_sub(usize::from(window.before) + usize::from(window.after));
    let mut used = 0;
    let mut end = start;
    for (byte, grapheme) in source[start..].grapheme_indices(true) {
        let cells = UnicodeWidthStr::width(grapheme);
        if used + cells > available {
            break;
        }
        used += cells;
        end = start + byte + grapheme.len();
    }
    window.after &= end < source.len();
    window.bytes = start..end;
    window
}

fn append_search_window(
    spans: &mut Vec<Span<'static>>,
    source: &str,
    first: std::ops::Range<usize>,
    width: usize,
    style: Style,
    matcher: Option<&crate::matching::LiteralMatcher>,
    matched_style: Style,
) {
    use unicode_segmentation::UnicodeSegmentation;
    let window = search_window(source, first.clone(), width);
    let start = window.bytes.start;
    let end = window.bytes.end;
    if window.before {
        spans.push(Span::styled("…", style));
    }
    let ranges = matcher.map_or_else(Vec::new, |matcher| matcher.ranges(source));
    for (byte, grapheme) in source[start..end].grapheme_indices(true) {
        let byte = byte + start;
        let matched = matcher.is_some()
            && std::iter::once(&first)
                .chain(ranges.iter())
                .any(|range| range.start < byte + grapheme.len() && byte < range.end);
        let style = if matched { matched_style } else { style };
        if let Some(last) = spans.last_mut().filter(|last| last.style == style) {
            last.content.to_mut().push_str(grapheme);
        } else {
            spans.push(Span::styled(grapheme.to_owned(), style));
        }
    }
    if window.after {
        spans.push(Span::styled("…", style));
    }
}

/// Number only the first visual row of each original line.
fn numbered_source_rows(
    notices: &[Line<'static>],
    lines: &[Line<'static>],
    width: u16,
    palette: &Palette,
) -> (Vec<Line<'static>>, Vec<usize>) {
    let gutter = source_gutter_width(lines.len(), width);
    let text_width = width.saturating_sub(gutter).max(1);
    let (mut rows, _) = wrap_text_lines(notices.iter(), width, 0, usize::MAX, true);
    let mut sources = vec![0; rows.len()];
    for (index, line) in lines.iter().enumerate() {
        let (wrapped, _) = wrap_text_lines(std::iter::once(line), text_width, 0, usize::MAX, true);
        for (continuation, mut row) in wrapped.into_iter().enumerate() {
            if gutter > 0 {
                let label = if continuation == 0 {
                    format!("{:>digits$} │", index + 1, digits = gutter as usize - 2)
                } else {
                    " ".repeat(gutter as usize)
                };
                row.spans
                    .insert(0, Span::styled(label, muted_style(palette)));
            }
            rows.push(row);
            sources.push(index + 1);
        }
    }
    (rows, sources)
}

fn source_gutter_width(lines: usize, width: u16) -> u16 {
    let wanted = lines.max(1).ilog10() as u16 + 3;
    // On tiny surfaces the gutter must not consume the readable text area.
    if width >= wanted.saturating_add(4) {
        wanted
    } else {
        0
    }
}

pub(crate) fn source_line_offset(app: &App, width: u16, line: usize) -> usize {
    viewer_text(app, width, &app.theme().palette())
        .source_rows
        .iter()
        .position(|source| *source == line)
        .unwrap_or(0)
}

pub(crate) fn viewer_source_at_scroll(app: &App) -> Option<usize> {
    let cache = app.viewer_text_cache().borrow();
    cache
        .as_ref()?
        .source_rows
        .get(app.viewer_scroll())
        .copied()
        .filter(|line| *line > 0)
}

#[derive(Debug)]
struct EditorCell {
    text: String,
    start: usize,
    end: usize,
    width: usize,
}

#[derive(Debug, Default)]
struct EditorRow {
    cells: Vec<EditorCell>,
    start: usize,
    end: usize,
}

#[derive(Debug, Default)]
struct EditorLine {
    source: String,
    rows: Vec<EditorRow>,
    terminated: bool,
    styles: Vec<crate::document::SourceStyleRange>,
    outgoing: Option<crate::document::SourceState>,
}

#[derive(Debug)]
pub(crate) struct EditorText {
    width: u16,
    revision: u64,
    syntax: Option<crate::document::SourceSyntax>,
    palette: Option<Palette>,
    lines: Vec<EditorLine>,
    scroll: usize,
    manual_scroll: bool,
}

impl EditorLine {
    fn style_at(&self, byte: usize, palette: &Palette) -> Style {
        let index = self.styles.partition_point(|range| range.bytes.end <= byte);
        self.styles
            .get(index)
            .filter(|range| range.bytes.contains(&byte))
            .map_or_else(
                || Style::default().fg(palette.syntax.text),
                |range| range.style,
            )
    }
}

impl EditorText {
    fn new() -> Self {
        Self {
            width: 0,
            revision: u64::MAX,
            syntax: None,
            palette: None,
            lines: Vec::new(),
            scroll: 0,
            manual_scroll: false,
        }
    }

    fn update(&mut self, editor: &crate::editor::Editor, width: u16, palette: &Palette) {
        let changed = self.revision != editor.revision();
        let adjacent = self.syntax.is_some() && self.revision.wrapping_add(1) == editor.revision();
        let syntax = match self.syntax {
            Some(syntax) if !changed || (adjacent && editor.changed_line() != 0) => syntax,
            _ => crate::document::SourceSyntax::for_path(
                editor.path(),
                editor.lines().first().map_or("", String::as_str),
            ),
        };
        let restyle = self
            .palette
            .is_none_or(|old| old.syntax != palette.syntax || old.markdown != palette.markdown);
        let reset = self.syntax != Some(syntax) || restyle || (changed && !adjacent);
        let resized = self.width != width;
        if changed || reset {
            let first = if reset { 0 } else { editor.changed_line() };
            // A replacement can change several lines with no net line-count change.
            // Align the untouched tail, then locate all changed source before convergence.
            if editor.lines().len() > self.lines.len() {
                let gap = (first + 1).min(self.lines.len());
                let extra = editor.lines().len() - self.lines.len();
                self.lines.splice(
                    gap..gap,
                    std::iter::repeat_with(EditorLine::default).take(extra),
                );
            } else if editor.lines().len() < self.lines.len() {
                let removed = self.lines.len() - editor.lines().len();
                // Keep the old ending-line boundary beside the untouched tail.
                // Keeping the starting line can falsely converge after a merged line.
                self.lines.drain(first..first + removed);
            }
            let last = editor.lines().len() - 1;
            let suffix = (first..editor.lines().len())
                .rfind(|&index| {
                    let cached = &self.lines[index];
                    cached.outgoing.is_none()
                        || cached.source != editor.lines()[index]
                        || cached.terminated != (index < last)
                })
                .map_or(first, |index| index + 1);
            let mut state = if first == 0 {
                syntax.state()
            } else {
                self.lines[first - 1].outgoing.as_ref().unwrap().clone()
            };
            // Syntect's newline grammars need a break, but editor source excludes it.
            // Reuse one scratch line rather than concatenate or copy the whole buffer.
            let mut input = String::new();
            for (index, source) in editor.lines().iter().enumerate().skip(first) {
                let cached = &mut self.lines[index];
                let terminated = index < last;
                let same_source = cached.source == *source && cached.terminated == terminated;
                let parser_input = if terminated {
                    input.clear();
                    input.push_str(source);
                    input.push('\n');
                    input.as_str()
                } else {
                    source.as_str()
                };
                let styles = state.line(parser_input, source.len(), palette);
                let converged = same_source && cached.outgoing.as_ref() == Some(&state);
                if cached.source != *source || cached.rows.is_empty() {
                    cached.source.clone_from(source);
                    if !resized {
                        cached.rows = editor_line_rows(source, width as usize);
                    }
                }
                cached.terminated = terminated;
                cached.styles = styles;
                if !converged {
                    cached.outgoing = Some(state.clone());
                }
                if !reset && index >= suffix && converged {
                    break;
                }
            }
        }
        if resized {
            for cached in &mut self.lines {
                cached.rows = editor_line_rows(&cached.source, width as usize);
            }
        }
        self.width = width;
        self.revision = editor.revision();
        self.syntax = Some(syntax);
        self.palette = Some(*palette);
    }
}

fn editor_line_rows(source: &str, width: usize) -> Vec<EditorRow> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut rows = Vec::new();
    let mut row = EditorRow::default();
    let mut used = 0;
    let mut source_column = 0;
    for (start, grapheme) in source.grapheme_indices(true) {
        let text = if grapheme == "\t" {
            " ".repeat(
                crate::editor::EDITOR_TAB_WIDTH - source_column % crate::editor::EDITOR_TAB_WIDTH,
            )
        } else {
            grapheme
                .chars()
                .map(|ch| if ch.is_control() { '\u{fffd}' } else { ch })
                .collect()
        };
        source_column += UnicodeWidthStr::width(text.as_str());
        let cells = UnicodeWidthStr::width(text.as_str()).min(width);
        if used > 0 && used + cells > width {
            row.end = start;
            rows.push(row);
            row = EditorRow {
                start,
                ..EditorRow::default()
            };
            used = 0;
        }
        row.cells.push(EditorCell {
            text,
            start,
            end: start + grapheme.len(),
            width: cells,
        });
        row.end = start + grapheme.len();
        used += cells;
        if used >= width {
            rows.push(row);
            row = EditorRow {
                start: start + grapheme.len(),
                end: start + grapheme.len(),
                ..EditorRow::default()
            };
            used = 0;
        }
    }
    rows.push(row);
    rows
}

fn render_file_editor(frame: &mut Frame<'_>, app: &App, area: Rect, palette: &Palette) {
    let editor = app.editor().unwrap();
    frame.render_widget(Clear, area);
    if area.is_empty() {
        return;
    }
    let [header, body, footer] = file_editor_areas(app, area);
    if app.is_viewing() {
        let outer = viewer_outer(area);
        if viewer_inner(outer) != outer {
            frame.render_widget(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(muted_style(palette)),
                outer,
            );
        }
    }
    frame.render_widget(
        Paragraph::new(shorten_left(
            &format!(
                " edit{} · {}",
                if editor.dirty() { "*" } else { "" },
                editor.path().display()
            ),
            header.width as usize,
        ))
        .style(
            Style::default()
                .fg(palette.title)
                .add_modifier(Modifier::BOLD),
        ),
        header,
    );
    let gutter = source_gutter_width(editor.lines().len(), body.width);
    let width = body.width.saturating_sub(gutter).max(1);
    let mut cache = app.editor_cache().borrow_mut();
    let text = cache.get_or_insert_with(EditorText::new);
    text.update(editor, width, palette);
    let (cursor_line, cursor_byte) = editor.cursor();
    let mut cursor_row = 0;
    let mut cursor_column = 0;
    for (line, cached) in text.lines.iter().enumerate() {
        if line == cursor_line {
            let row = cached
                .rows
                .iter()
                .rposition(|row| row.start <= cursor_byte)
                .unwrap_or(0);
            cursor_row += row;
            cursor_column = cached.rows[row]
                .cells
                .iter()
                .take_while(|cell| cell.end <= cursor_byte)
                .map(|cell| cell.width)
                .sum::<usize>();
            break;
        }
        cursor_row += cached.rows.len();
    }
    if !text.manual_scroll {
        if cursor_row < text.scroll {
            text.scroll = cursor_row;
        }
        if cursor_row >= text.scroll.saturating_add(body.height as usize) {
            text.scroll = cursor_row
                .saturating_add(1)
                .saturating_sub(body.height as usize);
        }
    }
    let maximum = text
        .lines
        .iter()
        .map(|line| line.rows.len())
        .sum::<usize>()
        .saturating_sub(body.height as usize);
    text.scroll = text.scroll.min(maximum);
    let selection = editor
        .selection()
        .map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
    let mut visual = 0;
    let mut visible = Vec::with_capacity(body.height as usize);
    'lines: for (index, cached) in text.lines.iter().enumerate() {
        for (continuation, row) in cached.rows.iter().enumerate() {
            if visual >= text.scroll && visible.len() < body.height as usize {
                let label = if gutter == 0 {
                    String::new()
                } else if continuation == 0 {
                    format!("{:>digits$} │", index + 1, digits = gutter as usize - 2)
                } else {
                    " ".repeat(gutter as usize)
                };
                let mut spans = vec![Span::styled(label, muted_style(palette))];
                spans.extend(row.cells.iter().map(|cell| {
                    Span::styled(
                        cell.text.clone(),
                        if selection.is_some_and(|(a, b)| {
                            (index, cell.start) >= a && (index, cell.start) < b
                        }) {
                            emphasis_style(palette)
                        } else {
                            cached.style_at(cell.start, palette)
                        },
                    )
                }));
                // A selected newline remains visible even when the line is empty.
                if selection.is_some_and(|(a, b)| (index, row.end) >= a && (index, row.end) < b)
                    && row.end == cached.source.len()
                {
                    spans.push(Span::styled(" ", emphasis_style(palette)));
                }
                visible.push(Line::from(spans));
            }
            visual += 1;
            if visible.len() >= body.height as usize {
                break 'lines;
            }
        }
    }
    frame.render_widget(Paragraph::new(visible), body);
    if let Some(track) = file_editor_track(app, area, body) {
        render_scrollbar(
            frame,
            scrollbar_geometry(track, text.scroll, maximum, body.height as usize),
            palette,
        );
    }
    if body.height > 0
        && body.width > 0
        && cursor_row >= text.scroll
        && cursor_row < text.scroll + body.height as usize
    {
        frame.set_cursor_position((
            body.x + gutter + (cursor_column as u16).min(width - 1),
            body.y
                + cursor_row
                    .saturating_sub(text.scroll)
                    .min(body.height as usize - 1) as u16,
        ));
    }
    let hint = if editor.confirming() {
        " Save changes?  s Save  d Discard  c/Esc Cancel"
    } else {
        " ^S save  Esc/^Q back  Shift+arrows select  ^Z undo  ^Y redo  ^C/^X/^V copy/cut/paste"
    };
    let status = editor.message().unwrap_or(hint);
    frame.render_widget(Paragraph::new(status).style(muted_style(palette)), footer);
}

fn viewer_text<'a>(app: &'a App, width: u16, palette: &Palette) -> std::cell::Ref<'a, ViewerText> {
    let reading = app.is_reading();
    let cache = app.viewer_text_cache();
    let needs_rebuild = cache.borrow().as_ref().is_none_or(|text| {
        text.width != width || text.reading != reading || text.browser_query != app.search_query()
    });
    if needs_rebuild {
        let (rows, source_rows, geometry) = if app
            .preview()
            .document()
            .is_some_and(|document| document.source_lines)
        {
            let document = app.preview().document().unwrap();
            let notices: Vec<_> = app
                .preview()
                .message()
                .into_iter()
                .chain(document.notice.as_deref())
                .map(|text| Line::styled(text.to_owned(), muted_style(palette)))
                .collect();
            source_geometry(document, width, reading, &notices, palette)
        } else {
            let (rows, _) = preview_text_lines(app, width, palette, 0, usize::MAX, true);
            (rows, Vec::new(), Vec::new())
        };
        *cache.borrow_mut() = Some(ViewerText {
            width,
            reading,
            rows,
            source_rows,
            geometry,
            browser_query: app.search_query().to_owned(),
            browser_matches: app.preview().document().map_or_else(Vec::new, |doc| {
                crate::matching::LiteralMatcher::new(app.search_query()).ranges(&doc.source)
            }),
        });
    }
    std::cell::Ref::map(cache.borrow(), |cache| cache.as_ref().unwrap())
}

pub fn viewer_scroll_max(app: &App, area: Rect) -> usize {
    if app.is_rendered_markdown_view() {
        return app.markdown_scroll_max();
    }
    let body = viewer_body(area);
    viewer_text(app, body.width, &app.theme().palette())
        .rows
        .len()
        .saturating_sub(body.height as usize)
}

fn render_viewer(frame: &mut Frame<'_>, app: &App, area: Rect, palette: &Palette) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let outer = viewer_outer(area);
    let inner = viewer_inner(outer);
    let body = viewer_body(area);
    if inner != outer {
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_style(muted_style(palette)),
            outer,
        );
    }
    let markdown = app
        .preview()
        .document()
        .is_some_and(|document| document.markdown_source.is_some());
    if area.height >= 3 {
        let [header, _, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(area);
        let size = if app.viewer_expanded() {
            "full"
        } else {
            "inline"
        };
        let heading = if markdown {
            let mode = if app.is_markdown_source() {
                "source"
            } else {
                "FMD rendered"
            };
            if app.theme_sample() != crate::ThemeSample::Picker {
                format!(" {} · {mode} · theme sample", app.theme().name())
            } else {
                format!(" {size} · {mode} · {}", app.preview().label())
            }
        } else if app.supports_reading() {
            let mode = if app.is_reading() { "reading" } else { "raw" };
            format!(" {size} · {mode} · {}", app.preview().label())
        } else {
            format!(" {size} · view · {}", app.preview().label())
        };
        frame.render_widget(
            Paragraph::new(shorten_right(&heading, header.width as usize)).style(
                Style::default()
                    .fg(palette.title)
                    .add_modifier(Modifier::BOLD),
            ),
            header,
        );
        let find_status = app.find_status();
        frame.render_widget(
            Paragraph::new(if let Some(status) = app.status() {
                status
            } else if let Some(status) = find_status.as_deref() {
                status
            } else if app.theme_sample() != crate::ThemeSample::Picker && footer.width >= 64 {
                " tab sample  m mode  ←→/↑↓ theme  pg↑↓ scroll  enter save  esc cancel"
            } else if app.theme_sample() != crate::ThemeSample::Picker {
                " tab sample  m mode  arrows theme  ↵ save  esc"
            } else if markdown && footer.width >= 72 {
                " f size  / find  n/N next  y copy  e edit  m mode  q back  ↑↓ scroll"
            } else if markdown && footer.width >= 42 {
                " f size  e edit  m mode  q back  ↑↓"
            } else if markdown {
                "f size  m mode  q back"
            } else if app.supports_reading() && footer.width >= 64 {
                " f size  / find  n/N next  y copy  e edit  R mode  q back  ↑↓"
            } else if app.supports_reading() && footer.width >= 42 {
                " f size  e edit  R mode  q back  ↑↓"
            } else if app.supports_reading() {
                "f size  R mode  q back"
            } else if footer.width >= 64 {
                " f size  / find  n/N next  y copy  e edit  q back  ↑↓ scroll"
            } else if footer.width >= 30 {
                " f size  e edit  q back  ↑↓"
            } else {
                "f size  q back"
            })
            .style(muted_style(palette)),
            footer,
        );
    }
    if let Some(track) = viewer_track(area) {
        let scrollbar = viewer_scrollbar(app, area);
        let lines = (0..track.height)
            .map(|row| {
                let y = track.y + row;
                if scrollbar.is_some_and(|bar| y >= bar.thumb.y && y < bar.thumb.bottom()) {
                    Line::styled("┃", Style::default().fg(palette.accent))
                } else {
                    Line::styled("│", muted_style(palette))
                }
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), track);
    }
    if app.is_rendered_markdown_view() {
        if let Some(message) = app.markdown_message() {
            frame.render_widget(
                Paragraph::new(message).wrap(ratatui::widgets::Wrap { trim: false }),
                body,
            );
        }
        return;
    }
    let cached = viewer_text(app, body.width, palette);
    let lines = (app.viewer_scroll()..cached.rows.len())
        .take(body.height as usize)
        .map(|row| highlighted_source_row(app, &cached, row, palette, app.viewer_matches()))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), body);
}

fn render_scrollbar(frame: &mut Frame<'_>, bar: ViewerScrollBar, palette: &Palette) {
    let lines = (0..bar.track.height)
        .map(|row| {
            let y = bar.track.y + row;
            if y >= bar.thumb.y && y < bar.thumb.bottom() {
                Line::styled("┃", Style::default().fg(palette.accent))
            } else {
                Line::styled("│", muted_style(palette))
            }
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), bar.track);
}

fn render_footer(frame: &mut Frame<'_>, app: &App, area: Rect, palette: &Palette) {
    if app.is_searching() {
        frame.render_widget(search_footer(app, area.width as usize, palette), area);
        return;
    }
    let line = if area.width >= 85 {
        format!(
            " ↑↓ select  → browse  ← parent  / search  e edit  ↵ choose/view  ^G current  s:{}{}  . hidden:{}",
            app.sort_mode().label(),
            app.sort_direction().symbol(),
            if app.show_hidden() { "on" } else { "off" }
        )
    } else if area.width >= 48 {
        " ↑↓  → browse  / search  e edit  ↵ choose/view  ^G current".to_owned()
    } else {
        " / find  e edit  ↵ choose/view  ^G here".to_owned()
    };
    frame.render_widget(Paragraph::new(line).style(muted_style(palette)), area);
}

fn search_footer(app: &App, width: usize, palette: &Palette) -> Paragraph<'static> {
    use crate::search::SearchScope;
    let label = app.search_scope().label();
    let short_label = match app.search_scope() {
        SearchScope::Names => "Names",
        _ => label,
    };
    let limit = if app.search_truncated() { " LIMIT" } else { "" };
    let budget = width.saturating_sub(limit.len());
    let (label, control) = [
        (label, " Shift+Tab"),
        (short_label, " Shift+Tab"),
        (short_label, " ⇧Tab"),
        (&short_label[..1], " ⇧Tab"),
    ]
    .into_iter()
    .find(|(label, control)| label.len() + 2 + UnicodeWidthStr::width(*control) <= budget)
    .unwrap_or((&short_label[..1], " ⇧Tab"));
    let mut used = label.len() + 2 + UnicodeWidthStr::width(control);
    let mut spans = vec![
        Span::styled(format!("[{label}]"), emphasis_style(palette)),
        key(control, palette),
    ];
    let (names, lines, files) = app.search_counts();
    let mut append = |full: String, compact: String| {
        let remaining = budget.saturating_sub(used + 3);
        let (separator, text) = if UnicodeWidthStr::width(full.as_str()) <= remaining {
            (" · ", full)
        } else if UnicodeWidthStr::width(compact.as_str()) <= remaining {
            (" · ", compact)
        } else if UnicodeWidthStr::width(compact.as_str()) <= budget.saturating_sub(used + 1) {
            (" ", compact)
        } else {
            return;
        };
        used += UnicodeWidthStr::width(separator) + UnicodeWidthStr::width(text.as_str());
        spans.push(hint(format!("{separator}{text}"), palette));
    };
    append(
        format!("{names} names · {lines} lines in {files} files"),
        format!("N:{names} L:{lines} F:{files}"),
    );
    append(
        if app.search_pending() {
            "searching…"
        } else {
            "complete"
        }
        .to_owned(),
        if app.search_pending() { "…" } else { "done" }.to_owned(),
    );
    append(
        format!("{} paths scanned", app.search_scanned()),
        format!("S:{}", app.search_scanned()),
    );
    append("Esc restore".to_owned(), "Esc".to_owned());
    if !limit.is_empty() {
        spans.push(key(limit, palette));
    }
    Paragraph::new(Line::from(spans))
}

fn render_theme_footer(frame: &mut Frame<'_>, app: &App, area: Rect, palette: &Palette) {
    let line = if area.width >= 48 {
        Line::from(vec![
            key(" ←→/↑↓", palette),
            hint(" preview  ", palette),
            key("tab", palette),
            hint(" sample  ", palette),
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
            key("tab", palette),
            hint(" sample ", palette),
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
    fn viewer_wraps_and_scrolls_text_at_the_full_width() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("notes.txt"), "abcdefghij\nsecond\nlast").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_viewer();
        let area = Rect::new(0, 0, 5, 4);
        assert!(viewer_scroll_max(&app, area) >= 2);
        app.set_viewer_scroll(viewer_scroll_max(&app, area));
        let mut terminal = Terminal::new(TestBackend::new(5, 4)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert!(terminal.backend().to_string().contains("last"));
        assert_eq!(viewer_scroll_max(&app, Rect::new(0, 0, 20, 10)), 0);
    }

    #[test]
    fn reading_reflows_prose_but_preserves_raw_lines_and_block_boundaries() {
        let temp = tempdir().unwrap();
        let source = "alpha  \nbeta\n\n  block one\n  block two\ngamma\ndelta";
        fs::write(temp.path().join("notes.txt"), source).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_viewer();
        let area = Rect::new(0, 0, 23, 6);
        let palette = app.theme().palette();
        let text = |app: &App| {
            viewer_text(app, viewer_body(area).width, &palette)
                .rows
                .iter()
                .map(|row| {
                    row.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        };
        let raw_rows = text(&app);
        assert_eq!(raw_rows.len(), source.split('\n').count());
        assert!(
            raw_rows
                .iter()
                .zip(source.split('\n'))
                .all(|(row, line)| row.ends_with(line))
        );
        assert_eq!(
            viewer_text(&app, viewer_body(area).width, &palette).source_rows,
            (1..=7).collect::<Vec<_>>()
        );
        let raw_max = viewer_scroll_max(&app, area);
        app.set_viewer_scroll(raw_max);
        app.toggle_reading();
        assert_eq!(app.viewer_scroll(), 0);
        assert_eq!(
            text(&app),
            [
                "alpha beta",
                "",
                "  block one",
                "  block two",
                "gamma delta"
            ]
        );
        let reading_max = viewer_scroll_max(&app, area);
        assert!(reading_max < raw_max);
        assert_eq!(viewer_scrollbar(&app, area).unwrap().maximum, reading_max);
        app.toggle_reading();
        assert_eq!(text(&app), raw_rows);
        assert_eq!(
            fs::read_to_string(temp.path().join("notes.txt")).unwrap(),
            source
        );
    }

    #[test]
    fn wrapping_respects_words_styles_graphemes_and_indentation() {
        let red = Style::default().fg(ratatui::style::Color::Red);
        let input = [
            Line::from(vec![Span::raw("one "), Span::styled("two café", red)]),
            Line::raw("    indented"),
            Line::raw("e\u{301}界 x"),
        ];
        let (rows, count) = wrap_text_lines(input.iter(), 7, 0, 20, true);
        let text = rows
            .iter()
            .map(|row| {
                row.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            text,
            ["one two", "café", "    ind", "ented", "e\u{301}界 x"]
        );
        assert_eq!(count, text.len());
        assert_eq!(rows[0].spans.last().unwrap().style.fg, red.fg);
        let (wide, _) = wrap_text_lines(input.iter(), 20, 0, 20, true);
        assert_eq!(wide.len(), 3);
        let (visible, total) = wrap_text_lines(input.iter(), 7, 1, 1, false);
        assert_eq!(visible[0].spans[0].content, "café");
        assert_eq!(total, 2);
    }

    #[test]
    fn trailing_whitespace_does_not_add_wrapped_rows() {
        let input = [Line::raw("abcd  "), Line::raw("abc "), Line::raw("ab   ")];
        let (rows, count) = wrap_text_lines(input.iter(), 4, 0, 10, true);
        let text = rows
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(count, 3);
        assert_eq!(text, ["abcd", "abc ", "ab  "]);
    }

    #[test]
    fn viewer_reuses_wrapped_rows_for_scrolling_and_rewraps_on_resize() {
        let temp = tempdir().unwrap();
        fs::write(
            temp.path().join("notes.txt"),
            "one two three four five six seven eight nine",
        )
        .unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_viewer();
        let area = Rect::new(0, 0, 10, 6);
        assert!(viewer_scroll_max(&app, area) > 0);
        let cached_rows = app
            .viewer_text_cache()
            .borrow()
            .as_ref()
            .unwrap()
            .rows
            .as_ptr();
        app.set_viewer_scroll(1);
        let mut terminal = Terminal::new(TestBackend::new(10, 6)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let body = viewer_body(area);
        let cached = viewer_text(&app, body.width, &app.theme().palette());
        let expected = cached.rows[1].to_string();
        let actual: String = (body.x..body.right())
            .map(|x| terminal.backend().buffer()[(x, body.y)].symbol())
            .collect();
        assert_eq!(actual.trim_end(), expected.trim_end());
        drop(cached);
        assert_eq!(
            cached_rows,
            app.viewer_text_cache()
                .borrow()
                .as_ref()
                .unwrap()
                .rows
                .as_ptr()
        );
        let narrow_count = app
            .viewer_text_cache()
            .borrow()
            .as_ref()
            .unwrap()
            .rows
            .len();
        viewer_scroll_max(&app, Rect::new(0, 0, 30, 6));
        let cache = app.viewer_text_cache().borrow();
        let cache = cache.as_ref().unwrap();
        assert_eq!(cache.width, 27);
        assert!(cache.rows.len() < narrow_count);
    }

    #[test]
    fn browser_sample_scroll_uses_inner_border_height() {
        let area = Rect::new(0, 0, 40, 6);
        assert_eq!(browser_sample_page_rows(area), 2);
        assert_eq!(browser_sample_page_rows(Rect::new(0, 0, 2, 4)), 2);
        let temp = tempdir().unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.set_viewer_scroll(browser_sample_scroll_max(browser_sample_page_rows(area)));
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_theme_preview(frame, &app))
            .unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Selected file"));
        assert!(rendered.contains("Selected symlink"));
    }

    #[test]
    fn viewer_border_and_page_rows_follow_terminal_size() {
        let area = Rect::new(2, 3, 40, 12);
        assert_eq!(viewer_body(area), Rect::new(3, 5, 37, 8));
        assert_eq!(viewer_page_rows(area), 8);
        assert_eq!(viewer_page_rows(Rect::new(0, 0, 2, 2)), 2);
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("notes.txt"), "hello").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_viewer();
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains('┌'));
        assert!(rendered.contains('┘'));
        assert!(rendered.contains("│hello"));
        assert_eq!(viewer_page_rows(Rect::new(0, 0, 40, 7)), 3);
    }

    #[test]
    fn viewer_expansion_shows_size_and_key_at_narrow_and_wide_widths() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("notes.txt"), "line\n".repeat(180)).unwrap();
        for width in [12, 40, 112] {
            let mut app = App::new(temp.path().to_path_buf()).unwrap();
            app.open_viewer();
            for (expanded, height, label) in [(false, 30, "inline"), (true, 48, "full")] {
                if expanded {
                    app.toggle_viewer_expanded();
                }
                let area = Rect::new(0, 0, width, height);
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| render(frame, &app)).unwrap();
                let screen = terminal.backend().to_string();
                assert!(screen.contains(label), "{screen}");
                assert!(screen.contains("f size"), "{screen}");
                assert_eq!(viewer_page_rows(area), usize::from(height - 4));
                let bar = viewer_scrollbar(&app, area).unwrap();
                assert_eq!(bar.track.height, height - 4);
            }
        }
    }

    #[test]
    fn scrollbar_geometry_preserves_endpoints_and_grab_offset() {
        let track = Rect::new(8, 4, 1, 10);
        let bar = scrollbar_geometry(track, 45, 90, 10);
        assert_eq!(bar.thumb, Rect::new(8, 8, 1, 1));
        assert_eq!(bar.scroll_at(0, 0), 0);
        assert_eq!(bar.scroll_at(13, 0), 90);
        assert_eq!(bar.scroll_at(u16::MAX, 0), 90);
        let large_thumb = scrollbar_geometry(track, 0, 10, 10);
        assert_eq!(large_thumb.thumb.height, 5);
        assert_eq!(large_thumb.scroll_at(9, 2), 6);
        assert_eq!(large_thumb.scroll_at(13, 2), 10);
        let clamped = scrollbar_geometry(track, usize::MAX, 90, 10);
        assert_eq!(clamped.offset, 90);
        assert_eq!(clamped.thumb.bottom(), track.bottom());
        let disabled = scrollbar_geometry(track, 20, 0, 10);
        assert_eq!(disabled.thumb, track);
        assert_eq!(disabled.offset, 0);
        assert_eq!(disabled.scroll_at(13, 0), 0);
    }

    #[test]
    fn scrollbar_handles_large_pixel_ranges_without_overflow() {
        let track = Rect::new(1, 2, 1, 100);
        let maximum = usize::MAX - 1;
        let bar = scrollbar_geometry(track, maximum, maximum, usize::MAX);
        assert_eq!(bar.thumb.height, 50);
        assert_eq!(bar.thumb.bottom(), track.bottom());
        assert_eq!(bar.scroll_at(track.bottom() - 1, 0), maximum);
        let long_page = scrollbar_geometry(track, 500_000, 1_000_000, 1_600);
        assert_eq!(long_page.thumb.height, 1);
        assert_eq!(long_page.thumb.y, track.y + 49);
    }

    #[test]
    fn scrollbar_and_body_share_tiny_area_boundaries() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("notes.txt"), "a\nb\nc").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_viewer();
        let one_column = Rect::new(0, 0, 1, 2);
        assert_eq!(viewer_body(one_column), one_column);
        assert!(viewer_scrollbar(&app, one_column).is_none());
        let no_rows = Rect::new(0, 0, 5, 0);
        assert!(viewer_scrollbar(&app, no_rows).is_none());
        let two_columns = Rect::new(0, 0, 2, 2);
        assert_eq!(viewer_body(two_columns), Rect::new(0, 0, 1, 2));
        assert_eq!(
            viewer_scrollbar(&app, two_columns).unwrap().track,
            Rect::new(1, 0, 1, 2)
        );
    }

    #[test]
    fn scrollbar_width_controls_text_and_markdown_source_wrapping() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("notes.md"), "abcdefghij").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_viewer();
        let area = Rect::new(0, 0, 11, 5);
        assert!(viewer_scrollbar(&app, area).is_none());
        app.toggle_markdown_source();
        let bar = viewer_scrollbar(&app, area).unwrap();
        assert_eq!(viewer_body(area).width, 8);
        assert_eq!(bar.maximum, 1);
        assert_eq!(bar.viewport, 1);
        let mut terminal = Terminal::new(TestBackend::new(11, 5)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let screen = terminal.backend().to_string();
        assert!(screen.contains("1 │abcde┃"));
        app.set_viewer_scroll(1);
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let continued = terminal.backend().to_string();
        assert!(continued.contains("   fghij┃"));
        assert!(!continued.contains("1 │fghij"));
    }

    #[test]
    fn markdown_preview_and_source_view_use_the_same_styled_rows() {
        let temp = tempdir().unwrap();
        fs::write(
            temp.path().join("notes.md"),
            "# Heading [site](https://example.org)\n\n- **bold** and *italic* `code`\n\n> quote\n",
        )
        .unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let palette = app.theme().palette();
        for width in [25, 80] {
            let preview = preview_text_lines(&app, width, &palette, 0, usize::MAX, true);
            app.open_viewer();
            app.toggle_markdown_source();
            let source = preview_text_lines(&app, width, &palette, 0, usize::MAX, true);
            assert_eq!(preview, source);
            app.close_viewer();
        }
    }

    #[test]
    fn markdown_viewer_switches_mode_and_content() {
        let temp = tempdir().unwrap();
        fs::write(
            temp.path().join("notes.md"),
            "# Heading\n\nSome **bold** text",
        )
        .unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_viewer();
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("rendered"));
        assert!(rendered.contains("m mode"));
        assert!(!rendered.contains("Some bold text"));
        assert!(!rendered.contains("**bold**"));
        app.toggle_markdown_source();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let source = terminal.backend().to_string();
        assert!(source.contains("source"));
        assert!(source.contains("# Heading"));
        assert!(source.contains("Some **bold** text"));
    }

    #[test]
    fn wide_render_shows_current_and_destination_panes() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("project")).unwrap();
        fs::write(temp.path().join("project/main.rs"), "fn main() {}").unwrap();
        fs::write(temp.path().join("README.md"), "hello").unwrap();
        let app = App::new(temp.path().to_path_buf()).unwrap();
        let backend = TestBackend::new(100, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("current"));
        assert!(rendered.contains("next · project/"));
        assert!(rendered.contains("main.rs"));
        assert!(rendered.contains('│'));
        let [_, body, _] = picker_areas(Rect::new(0, 0, 100, 8));
        let [left, _, right] = picker_panes(body);
        let pane_text = |area: Rect| {
            (area.y..area.bottom())
                .map(|y| {
                    (area.x..area.right())
                        .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(pane_text(left).contains("project/"));
        assert!(!pane_text(left).contains("main.rs"));
        assert!(pane_text(right).contains("main.rs"));
    }

    #[test]
    fn narrow_render_collapses_to_one_pane_without_wrapping() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("a-very-long-directory-name")).unwrap();
        fs::write(
            temp.path()
                .join("a-very-long-directory-name/child-only.txt"),
            "text",
        )
        .unwrap();
        let app = App::new(temp.path().to_path_buf()).unwrap();
        let backend = TestBackend::new(24, 5);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let rendered = terminal.backend().to_string();
        assert_eq!(terminal.backend().buffer().area.width, 24);
        assert_eq!(terminal.backend().buffer()[(0, 1)].symbol(), "›");
        assert!(!rendered.contains("child-only.txt"));
        for row in 2..4 {
            assert!(
                (0..24).all(|column| terminal.backend().buffer()[(column, row)].symbol() == " ")
            );
        }
    }

    #[test]
    fn search_groups_lines_and_keeps_controls_visible_at_browser_widths() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("Cargo-project")).unwrap();
        fs::write(temp.path().join("Cargo.toml"), "cargo first\nsecond cargo").unwrap();
        fs::write(temp.path().join("README.md"), "hello").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_search();
        for character in "cargo".chars() {
            app.push_search_char(character);
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let pending = terminal.backend().to_string();
        assert!(pending.contains("cargo"));
        assert!(!pending.contains("(empty)"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.search_pending() {
            app.poll_search();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        app.poll_search();
        for width in [112, 80, 40] {
            let mut terminal = Terminal::new(TestBackend::new(width, 10)).unwrap();
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let buffer = terminal.backend().buffer();
            let row = |y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            };
            assert_eq!(row(0).trim_end(), "/ cargo▏");
            let footer = row(9);
            assert!(footer.starts_with("[Both] Shift+Tab"));
            assert!(!footer.contains("Content") && !footer.contains("Files & folders"));
            if width >= 80 {
                assert!(
                    footer.contains("2 names")
                        && footer.contains("2 lines")
                        && footer.contains("1 files")
                );
            } else {
                assert!(footer.contains("N:2 L:2 F:1"));
            }
            assert!(footer.contains("complete") || footer.contains("done"));
            let [_, body, _] = picker_areas(Rect::new(0, 0, width, 10));
            let list = if width >= TWO_PANE_MIN_WIDTH {
                picker_panes(body)[0]
            } else {
                body
            };
            let text = (list.y..list.bottom())
                .map(|y| {
                    (list.x..list.right())
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(text.matches("Cargo.toml").count(), 1);
            assert!(text.contains("cargo first") && text.contains("second cargo"));
            assert!(!text.contains("README.md"));
        }
    }

    #[test]
    fn search_keeps_only_active_mode_control_in_footer_and_query_tail_in_header() {
        use crate::search::SearchScope;
        let temp = tempdir().unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_search();
        let query = format!("{}e\u{301}-tail", "界".repeat(80));
        for character in query.chars() {
            app.push_search_char(character);
        }
        for scope in [SearchScope::Both, SearchScope::Names, SearchScope::Content] {
            assert_eq!(app.search_scope(), scope);
            for width in [112, 80, 40, 28, 20, 16] {
                let mut terminal = Terminal::new(TestBackend::new(width, 3)).unwrap();
                terminal
                    .draw(|frame| {
                        let palette = app.theme().palette();
                        render_search_header(frame, &app, Rect::new(0, 0, width, 1), &palette);
                        render_footer(frame, &app, Rect::new(0, 2, width, 1), &palette);
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let row = |y| {
                    (0..width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                };
                let header = row(0);
                assert!(header.starts_with("/ …"));
                assert!(header.trim_end().ends_with("e\u{301}-tail▏"));
                assert!(!header.contains("Tab") && !header.contains(scope.label()));
                let footer = row(2);
                let label = if scope == SearchScope::Names && width < 27 {
                    "Names"
                } else {
                    scope.label()
                };
                let control = if width as usize >= label.len() + 12 {
                    "Shift+Tab"
                } else {
                    "⇧Tab"
                };
                assert!(footer.starts_with(&format!("[{label}] {control}")));
                for inactive in [SearchScope::Both, SearchScope::Names, SearchScope::Content] {
                    if inactive != scope {
                        assert!(!footer.contains(inactive.label()));
                    }
                }
                // Check visible control cells, not just text assembled before clipping.
                for x in 0..(label.len() + 2) as u16 {
                    assert_eq!(buffer[(x, 2)].fg, app.theme().palette().emphasis_foreground);
                    assert_eq!(buffer[(x, 2)].bg, app.theme().palette().emphasis_background);
                    assert!(buffer[(x, 2)].modifier.contains(Modifier::BOLD));
                }
                let control_end = label.len() + 3 + UnicodeWidthStr::width(control);
                assert!(control_end <= width as usize);
                assert_eq!(
                    buffer[((control_end - 1) as u16, 2)].fg,
                    app.theme().palette().accent
                );
            }
            app.cycle_search_scope();
        }
    }

    #[test]
    fn search_footer_keeps_result_limit_after_active_mode_at_narrow_widths() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("many.txt"), "needle\n".repeat(10_001)).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_search();
        for character in "needle".chars() {
            app.push_search_char(character);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while app.search_pending() {
            app.poll_search();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        app.poll_search();
        assert!(app.search_truncated());
        for width in [112, 80, 40, 28, 20, 16, 14] {
            let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    render_footer(frame, &app, area, &app.theme().palette());
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let text = (0..width)
                .map(|x| buffer[(x, 0)].symbol())
                .collect::<String>();
            let control = if width >= 22 {
                "[Both] Shift+Tab"
            } else if width >= 17 {
                "[Both] ⇧Tab"
            } else {
                "[B] ⇧Tab"
            };
            assert!(text.starts_with(control));
            assert!(text.trim_end().ends_with(" LIMIT"));
            let last = UnicodeWidthStr::width(text.trim_end()) as u16 - 1;
            assert_eq!(buffer[(last, 0)].fg, app.theme().palette().accent);
        }
    }

    #[test]
    fn picker_editor_keeps_wide_directory_pane_and_uses_real_body_geometry() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("keep-listed.txt"), "other").unwrap();
        fs::write(
            temp.path().join("notes.txt"),
            (0..40)
                .map(|line| format!("line-{line:02}\n"))
                .collect::<String>(),
        )
        .unwrap();
        for width in [TWO_PANE_MIN_WIDTH - 1, TWO_PANE_MIN_WIDTH, 100] {
            let mut app = App::new(temp.path().to_path_buf()).unwrap();
            app.move_last();
            app.open_editor();
            let area = Rect::new(0, 0, width, 12);
            let editor_area = file_editor_area(&app, area);
            let body = file_editor_areas(&app, editor_area)[1];
            let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let rendered = terminal.backend().to_string();
            assert_eq!(
                rendered.contains("keep-listed.txt"),
                width >= TWO_PANE_MIN_WIDTH
            );
            assert_eq!(editor_area == area, width < TWO_PANE_MIN_WIDTH);
            assert!(body.contains(terminal.get_cursor_position().unwrap()));
            let rows = editor_page_rows(&app, area);
            assert_eq!(rows, if width >= TWO_PANE_MIN_WIDTH { 8 } else { 10 });
            app.handle_editor_key(termina::event::KeyCode::PageDown.into(), rows);
            assert_eq!(app.editor().unwrap().cursor().0, rows);
            terminal.draw(|frame| render(frame, &app)).unwrap();
            assert!(body.contains(terminal.get_cursor_position().unwrap()));
            assert!(app.editor_cache().borrow().as_ref().unwrap().scroll > 0);
            app.handle_editor_key(termina::event::KeyCode::Escape.into(), rows);
            assert!(!app.is_viewing() && app.editor().is_none());
            assert_eq!(app.selected_entry().unwrap().name, "notes.txt");
        }
    }

    #[test]
    fn viewer_editor_stays_full_width_even_when_picker_could_show_two_panes() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("keep-listed.txt"), "other").unwrap();
        fs::write(
            temp.path().join("notes.md"),
            (0..40)
                .map(|line| format!("# Heading-{line:02}\n"))
                .collect::<String>(),
        )
        .unwrap();
        for source in [false, true] {
            for (width, height, expected_body) in [
                (100, 12, Rect::new(1, 2, 97, 8)),
                (8, 5, Rect::new(1, 2, 5, 1)),
                (3, 5, Rect::new(1, 2, 1, 1)),
                (2, 4, Rect::new(0, 1, 1, 2)),
                (1, 3, Rect::new(0, 1, 1, 1)),
                (1, 2, Rect::new(0, 1, 1, 0)),
                (1, 1, Rect::new(0, 1, 1, 0)),
            ] {
                let mut app = App::new(temp.path().to_path_buf()).unwrap();
                app.move_last();
                app.open_viewer();
                if source {
                    app.toggle_markdown_source();
                }
                app.open_editor();
                let area = Rect::new(0, 0, width, height);
                let [header, body, footer] = file_editor_areas(&app, area);
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| render(frame, &app)).unwrap();
                assert!(!terminal.backend().to_string().contains("keep-listed.txt"));
                assert_eq!(file_editor_area(&app, area), area);
                let rows = editor_page_rows(&app, area);
                assert_eq!(rows, body.height.max(1) as usize);
                assert_eq!(body, expected_body);
                assert!(body.is_empty() || body.intersection(header).is_empty());
                assert!(body.is_empty() || body.intersection(footer).is_empty());
                if !body.is_empty() {
                    assert_eq!(body, viewer_body(area));
                    assert!(body.contains(terminal.get_cursor_position().unwrap()));
                    let gutter =
                        source_gutter_width(app.editor().unwrap().lines().len(), body.width);
                    assert_eq!(
                        app.editor_cache().borrow().as_ref().unwrap().width,
                        body.width.saturating_sub(gutter).max(1)
                    );
                }
                let outer = viewer_outer(area);
                if viewer_inner(outer) != outer {
                    let buffer = terminal.backend().buffer();
                    assert_eq!(buffer[(outer.x, outer.y)].symbol(), "┌");
                    assert_eq!(buffer[(outer.right() - 1, outer.y)].symbol(), "┐");
                    assert_eq!(buffer[(outer.x, outer.bottom() - 1)].symbol(), "└");
                    assert_eq!(
                        buffer[(outer.right() - 1, outer.bottom() - 1)].symbol(),
                        "┘"
                    );
                    for y in body.y..body.bottom() {
                        assert_eq!(buffer[(outer.x, y)].symbol(), "│");
                        assert_eq!(buffer[(outer.right() - 1, y)].symbol(), "│");
                    }
                }
                app.handle_editor_key(termina::event::KeyCode::PageDown.into(), rows);
                assert_eq!(app.editor().unwrap().cursor().0, rows);
                terminal.draw(|frame| render(frame, &app)).unwrap();
                if !body.is_empty() {
                    assert!(body.contains(terminal.get_cursor_position().unwrap()));
                    assert!(app.editor_cache().borrow().as_ref().unwrap().scroll > 0);
                }
                app.handle_editor_key(termina::event::KeyCode::Escape.into(), rows);
                assert!(app.editor().is_none() && app.is_viewing());
                assert_eq!(app.is_markdown_source(), source);
            }
        }
    }

    fn syntax_editor(
        name: &str,
        source: &str,
    ) -> (
        tempfile::TempDir,
        crate::editor::Editor,
        EditorText,
        Palette,
    ) {
        let directory = tempdir().unwrap();
        let path = directory.path().join(name);
        fs::write(&path, source).unwrap();
        let editor = crate::editor::Editor::open(path, 1).unwrap();
        let mut palette = crate::Theme::auto().palette();
        palette.syntax.text = ratatui::style::Color::White;
        palette.syntax.comment = ratatui::style::Color::Cyan;
        palette.syntax.keyword = ratatui::style::Color::Red;
        palette.syntax.string = ratatui::style::Color::Blue;
        palette.syntax.number = ratatui::style::Color::Magenta;
        let mut cache = EditorText::new();
        cache.update(&editor, 30, &palette);
        (directory, editor, cache, palette)
    }

    fn editor_syntax_key(
        editor: &mut crate::editor::Editor,
        code: termina::event::KeyCode,
        modifiers: termina::event::Modifiers,
    ) {
        editor.handle_key(termina::event::KeyEvent::new(code, modifiers), 8);
    }

    fn assert_editor_syntax_matches_fresh(
        cache: &EditorText,
        editor: &crate::editor::Editor,
        palette: &Palette,
    ) {
        let mut fresh = EditorText::new();
        fresh.update(editor, cache.width, palette);
        assert_eq!(cache.lines.len(), fresh.lines.len());
        for (index, (cached, expected)) in cache.lines.iter().zip(&fresh.lines).enumerate() {
            assert_eq!(cached.source, expected.source, "source line {index}");
            assert_eq!(cached.terminated, expected.terminated, "break at {index}");
            assert_eq!(cached.styles, expected.styles, "styles at {index}");
        }
    }

    #[test]
    fn editor_syntax_matches_source_preview_associations_and_styles() {
        for (name, source) in [
            (
                "Cargo.lock",
                "[[package]]\nname = \"example\"\nversion = \"1\"\n",
            ),
            ("Dockerfile.dev", "FROM alpine\nRUN echo \"hello\"\n"),
            (
                "tsconfig.build.json",
                "{\n// note\n\"compilerOptions\": {}\n}\n",
            ),
            ("module.mts", "const value: number = 42;\n"),
            ("script", "#!/usr/bin/env python\nprint(\"hello\") # note\n"),
            ("notes.md", "# Heading\n```rust\nlet value = 42;\n```\n"),
            ("unknown.no-syntax-here", "plain source\n"),
        ] {
            let (_directory, editor, cache, palette) = syntax_editor(name, source);
            let preview = crate::document::load_source(editor.path(), &palette).unwrap();
            assert_eq!(cache.lines.len(), preview.lines.len(), "{name}");
            for (cached, line) in cache.lines.iter().zip(&preview.lines) {
                let mut offset = 0;
                for span in &line.spans {
                    for (byte, _) in span.content.char_indices() {
                        assert_eq!(
                            cached.style_at(offset + byte, &palette),
                            span.style,
                            "{name}"
                        );
                    }
                    offset += span.content.len();
                }
                assert_eq!(offset, cached.source.len(), "{name}");
            }
        }
    }

    #[test]
    fn editor_syntax_tracks_eof_breaks_without_changing_bom_or_crlf() {
        use termina::event::{KeyCode, Modifiers};
        let (_directory, mut editor, mut cache, palette) =
            syntax_editor("source.rs", "\u{feff}let value = 1;\r\n// tail");
        assert!(cache.lines[0].terminated);
        assert!(!cache.lines[1].terminated);
        assert!(editor.has_bom() && editor.uses_crlf());
        editor_syntax_key(&mut editor, KeyCode::End, Modifiers::CONTROL);
        editor_syntax_key(&mut editor, KeyCode::Enter, Modifiers::NONE);
        cache.update(&editor, 30, &palette);
        assert!(cache.lines[1].terminated);
        assert!(!cache.lines[2].terminated);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        for character in ['z', 'y'] {
            editor_syntax_key(&mut editor, KeyCode::Char(character), Modifiers::CONTROL);
            cache.update(&editor, 30, &palette);
            assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        }
        editor.save().unwrap();
        assert_eq!(
            fs::read(editor.path()).unwrap(),
            "\u{feff}let value = 1;\r\n// tail\r\n".as_bytes()
        );
    }

    #[test]
    fn editor_syntax_propagates_comments_and_survives_cursor_movement_and_resize() {
        use termina::event::{KeyCode, Modifiers};
        let (_directory, mut editor, mut cache, palette) = syntax_editor(
            "source.rs",
            "let first = 1;\nlet second = 2;\n*/ let third = 3;\nlet fourth = 4;\n",
        );
        editor_syntax_key(&mut editor, KeyCode::Right, Modifiers::NONE);
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        cache.update(&editor, 8, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        assert!(cache.lines[1].rows.len() > 1);
        assert_eq!(
            cache.lines[1]
                .rows
                .iter()
                .flat_map(|row| row.cells.iter().map(|cell| cell.text.as_str()))
                .collect::<String>(),
            editor.lines()[1]
        );
        editor_syntax_key(&mut editor, KeyCode::Home, Modifiers::NONE);
        editor.paste("/*");
        cache.update(&editor, 8, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        assert_eq!(
            cache.lines[1].style_at(0, &palette).fg,
            Some(palette.syntax.comment)
        );
        assert_eq!(
            cache.lines[3].style_at(0, &palette).fg,
            Some(palette.syntax.keyword)
        );
        editor_syntax_key(&mut editor, KeyCode::Char('z'), Modifiers::CONTROL);
        cache.update(&editor, 8, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        assert_eq!(
            cache.lines[1].style_at(0, &palette).fg,
            Some(palette.syntax.keyword)
        );
        editor_syntax_key(&mut editor, KeyCode::Char('y'), Modifiers::CONTROL);
        cache.update(&editor, 8, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        // A local edit after the closed comment must keep later source styles correct.
        editor_syntax_key(&mut editor, KeyCode::End, Modifiers::CONTROL);
        editor_syntax_key(&mut editor, KeyCode::Up, Modifiers::NONE);
        editor_syntax_key(&mut editor, KeyCode::Home, Modifiers::NONE);
        editor.paste(" ");
        cache.update(&editor, 8, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
    }

    #[test]
    fn editor_syntax_aligns_insert_delete_undo_and_missed_revisions() {
        use termina::event::{KeyCode, Modifiers};
        let (_directory, mut editor, mut cache, palette) = syntax_editor(
            "source.rs",
            "let first = 1;\nlet second = 2;\nlet third = 3;",
        );
        editor_syntax_key(&mut editor, KeyCode::Down, Modifiers::NONE);
        editor.paste("/* open\nbody\n");
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        assert_eq!(
            cache.lines[4].style_at(0, &palette).fg,
            Some(palette.syntax.comment)
        );
        for character in ['z', 'y', 'z'] {
            editor_syntax_key(&mut editor, KeyCode::Char(character), Modifiers::CONTROL);
            cache.update(&editor, 30, &palette);
            assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        }
        // Join two lines, then restore them through history.
        editor_syntax_key(&mut editor, KeyCode::Home, Modifiers::NONE);
        editor_syntax_key(&mut editor, KeyCode::Backspace, Modifiers::NONE);
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        editor_syntax_key(&mut editor, KeyCode::Char('z'), Modifiers::CONTROL);
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        // Edits between frames must still produce the same colors as a fresh preview.
        editor_syntax_key(&mut editor, KeyCode::Home, Modifiers::CONTROL);
        editor.paste("/*");
        editor_syntax_key(&mut editor, KeyCode::End, Modifiers::CONTROL);
        editor.paste("\n*/\n");
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        editor.save().unwrap();
        assert_eq!(
            fs::read_to_string(editor.path()).unwrap(),
            editor.lines().join("\n")
        );

        // Removing a comment between identical blank lines changes the unchanged tail.
        let (_directory, mut editor, mut cache, palette) =
            syntax_editor("source.rs", "let a = 1;\n\n/*\n\nlet b = 2;");
        assert_eq!(
            cache.lines[4].style_at(0, &palette).fg,
            Some(palette.syntax.comment)
        );
        editor_syntax_key(&mut editor, KeyCode::Down, Modifiers::NONE);
        editor_syntax_key(&mut editor, KeyCode::Down, Modifiers::SHIFT);
        editor_syntax_key(&mut editor, KeyCode::Down, Modifiers::SHIFT);
        editor_syntax_key(&mut editor, KeyCode::Delete, Modifiers::NONE);
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        assert_eq!(
            cache.lines[2].style_at(0, &palette).fg,
            Some(palette.syntax.keyword)
        );
        editor_syntax_key(&mut editor, KeyCode::Char('z'), Modifiers::CONTROL);
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        assert_eq!(
            cache.lines[4].style_at(0, &palette).fg,
            Some(palette.syntax.comment)
        );
        editor_syntax_key(&mut editor, KeyCode::Char('y'), Modifiers::CONTROL);
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        assert_eq!(
            cache.lines[2].style_at(0, &palette).fg,
            Some(palette.syntax.keyword)
        );

        // Joining an HTML block to its blank separator changes later Markdown.
        let (_directory, mut editor, mut cache, mut palette) =
            syntax_editor("notes.md", "<div>\n\n# Heading");
        palette.markdown.heading = ratatui::style::Color::Yellow;
        palette.markdown.text = ratatui::style::Color::White;
        cache.update(&editor, 30, &palette);
        assert_eq!(
            cache.lines[2].style_at(2, &palette).fg,
            Some(palette.markdown.heading)
        );
        editor_syntax_key(&mut editor, KeyCode::End, Modifiers::NONE);
        editor_syntax_key(&mut editor, KeyCode::Delete, Modifiers::NONE);
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        assert_ne!(
            cache.lines[1].style_at(2, &palette).fg,
            Some(palette.markdown.heading)
        );
        editor_syntax_key(&mut editor, KeyCode::Char('z'), Modifiers::CONTROL);
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        assert_eq!(
            cache.lines[2].style_at(2, &palette).fg,
            Some(palette.markdown.heading)
        );
    }

    #[test]
    fn editor_syntax_checks_all_replaced_lines_before_convergence() {
        use termina::event::{KeyCode, Modifiers};
        let (_directory, mut editor, mut cache, palette) = syntax_editor(
            "source.rs",
            "let first = 1;\nlet same = 2;\nlet last = 3;\n",
        );
        editor_syntax_key(&mut editor, KeyCode::Char('a'), Modifiers::CONTROL);
        editor.paste("let first = 9;\nlet same = 2;\n/* let last = 3;\n");
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        assert_eq!(
            cache.lines[2].style_at(0, &palette).fg,
            Some(palette.syntax.comment)
        );
    }

    #[test]
    fn editor_syntax_propagates_multiline_strings_and_fence_language_changes() {
        use termina::event::{KeyCode, Modifiers};
        let (_directory, mut editor, mut cache, palette) = syntax_editor(
            "source.rs",
            "let text = r#\"first\nsecond\n\"#;\nlet last = 1;\n",
        );
        assert_eq!(
            cache.lines[1].style_at(0, &palette).fg,
            Some(palette.syntax.string)
        );
        editor_syntax_key(&mut editor, KeyCode::End, Modifiers::NONE);
        editor.paste("\"#;");
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        assert_ne!(
            cache.lines[1].style_at(0, &palette).fg,
            Some(palette.syntax.string)
        );
        editor_syntax_key(&mut editor, KeyCode::Char('z'), Modifiers::CONTROL);
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);

        let (_directory, mut editor, mut cache, palette) = syntax_editor(
            "notes.md",
            "```rust\nlet value = 42; // note\n```\n# Heading\n",
        );
        let comment = editor.lines()[1].find("//").unwrap();
        assert_eq!(
            cache.lines[1].style_at(comment, &palette).fg,
            Some(palette.syntax.comment)
        );
        editor_syntax_key(&mut editor, KeyCode::End, Modifiers::SHIFT);
        editor.paste("```python");
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        assert_ne!(
            cache.lines[1].style_at(comment, &palette).fg,
            Some(palette.syntax.comment)
        );
        assert_eq!(
            cache.lines[3].style_at(2, &palette).fg,
            Some(palette.markdown.heading)
        );
        editor_syntax_key(&mut editor, KeyCode::Char('z'), Modifiers::CONTROL);
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        editor_syntax_key(&mut editor, KeyCode::End, Modifiers::SHIFT);
        editor.paste("```no-such-language");
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        assert_eq!(
            cache.lines[1].style_at(comment, &palette).fg,
            Some(palette.syntax.text)
        );
    }

    #[test]
    fn editor_syntax_invalidates_shebang_and_theme_without_changing_source() {
        use termina::event::{KeyCode, Modifiers};
        let (_directory, mut editor, mut cache, mut palette) = syntax_editor(
            "script",
            "#!/usr/bin/env python\n# note\nprint(\"hello\")\n",
        );
        let python = cache.syntax;
        assert_eq!(
            cache.lines[1].style_at(0, &palette).fg,
            Some(palette.syntax.comment)
        );
        palette.syntax.comment = ratatui::style::Color::Yellow;
        let source = editor.lines().to_vec();
        let revision = editor.revision();
        cache.update(&editor, 30, &palette);
        assert_eq!(
            cache.lines[1].style_at(0, &palette).fg,
            Some(palette.syntax.comment)
        );
        assert_eq!(editor.lines(), source);
        assert_eq!(editor.revision(), revision);
        editor_syntax_key(&mut editor, KeyCode::End, Modifiers::SHIFT);
        editor.paste("#!/bin/sh");
        cache.update(&editor, 30, &palette);
        assert_ne!(cache.syntax, python);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        editor_syntax_key(&mut editor, KeyCode::Home, Modifiers::NONE);
        editor_syntax_key(&mut editor, KeyCode::End, Modifiers::SHIFT);
        editor.paste("not a shebang");
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
        assert_eq!(
            cache.lines[1].style_at(0, &palette).fg,
            Some(palette.syntax.text)
        );
        editor_syntax_key(&mut editor, KeyCode::Char('z'), Modifiers::CONTROL);
        cache.update(&editor, 30, &palette);
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);

        let (_directory, editor, mut cache, mut palette) =
            syntax_editor("notes.md", "# Heading\n\n`code`\n");
        palette.markdown.heading = ratatui::style::Color::Yellow;
        palette.markdown.code = ratatui::style::Color::Green;
        cache.update(&editor, 30, &palette);
        assert_eq!(
            cache.lines[0].style_at(2, &palette).fg,
            Some(palette.markdown.heading)
        );
        assert_eq!(
            cache.lines[2].style_at(1, &palette).fg,
            Some(palette.markdown.code)
        );
        assert_editor_syntax_matches_fresh(&cache, &editor, &palette);
    }

    #[test]
    fn editor_syntax_uses_source_bytes_for_graphemes_and_selection_in_both_origins() {
        use ratatui::style::Color;
        use termina::event::{KeyCode, KeyEvent, Modifiers};
        let directory = tempdir().unwrap();
        let source =
            "let text = \"\t界e\u{301}\"\u{301}; // note with a long continuation for wrapping\n\n";
        fs::write(directory.path().join("source.rs"), source).unwrap();
        for viewer in [false, true] {
            let mut app = App::new(directory.path().to_path_buf()).unwrap();
            if viewer {
                app.open_viewer();
            }
            app.open_editor();
            let mut palette = app.theme().palette();
            palette.syntax.string = Color::Blue;
            palette.syntax.comment = Color::Cyan;
            palette.emphasis_foreground = Color::Black;
            palette.emphasis_background = Color::Yellow;
            palette.reverse_emphasis = false;
            let area = Rect::new(0, 0, 80, 16);
            let editor_area = file_editor_area(&app, area);
            let body = file_editor_areas(&app, editor_area)[1];
            let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            for selected in [false, true] {
                if selected {
                    app.handle_editor_key(KeyEvent::new(KeyCode::Char('a'), Modifiers::CONTROL), 8);
                }
                terminal
                    .draw(|frame| render_file_editor(frame, &app, editor_area, &palette))
                    .unwrap();
                let cache = app.editor_cache().borrow();
                let cache = cache.as_ref().unwrap();
                let gutter = source_gutter_width(cache.lines.len(), body.width);
                let buffer = terminal.backend().buffer();
                let mut y = body.y;
                for line in &cache.lines {
                    for row in &line.rows {
                        let mut x = body.x + gutter;
                        for cell in &row.cells {
                            let expected = if selected {
                                emphasis_style(&palette)
                            } else {
                                line.style_at(cell.start, &palette)
                            };
                            assert_eq!(buffer[(x, y)].fg, expected.fg.unwrap());
                            if selected {
                                assert_eq!(buffer[(x, y)].bg, palette.emphasis_background);
                            }
                            if cell.text == "界" {
                                assert_eq!(cell.width, 2);
                            } else if cell.text == "e\u{301}" {
                                assert_eq!(cell.end - cell.start, 3);
                                assert_eq!(cell.width, 1);
                                assert_eq!(buffer[(x, y)].symbol(), "e\u{301}");
                            } else if cell.text == "\"\u{301}" {
                                // The closing quote and following mark cross a token boundary.
                                assert_eq!(cell.width, 1);
                                assert_eq!(buffer[(x, y)].symbol(), "\"\u{301}");
                                if !selected {
                                    assert_eq!(buffer[(x, y)].fg, palette.syntax.string);
                                }
                            }
                            if line.source.as_bytes().get(cell.start) == Some(&b'\t') {
                                assert_eq!(cell.end - cell.start, 1);
                                if !selected {
                                    assert_eq!(buffer[(x, y)].fg, palette.syntax.string);
                                }
                            }
                            x += cell.width as u16;
                        }
                        if selected && row.end == line.source.len() && line.terminated {
                            assert_eq!(buffer[(x, y)].bg, palette.emphasis_background);
                        }
                        y += 1;
                    }
                }
                assert!(body.contains(terminal.get_cursor_position().unwrap()));
                assert_eq!(app.editor().unwrap().lines().join("\n"), source);
                assert_eq!(app.editor().unwrap().revision(), 0);
            }
        }
    }

    #[test]
    fn editor_rewraps_changed_lines_and_shows_new_empty_source_lines() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("notes.txt"), "one").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_editor();
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        app.handle_editor_key(termina::event::KeyCode::End.into(), 6);
        app.handle_editor_key(termina::event::KeyCode::Enter.into(), 6);
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("1 │one"));
        assert!(rendered.contains("2 │"));
        app.paste("λ pasted");
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert!(terminal.backend().to_string().contains("2 │λ pasted"));
    }

    #[test]
    fn source_gutters_number_only_first_wrapped_row_and_skip_notices() {
        let palette = crate::Theme::auto().palette();
        let notices = [Line::raw("notice")];
        let lines = [Line::raw("abcdefgh"), Line::raw("β next")];
        let (rows, sources) = numbered_source_rows(&notices, &lines, 7, &palette);
        assert_eq!(sources, vec![0, 1, 1, 2, 2]);
        let strings: Vec<_> = rows.iter().map(ToString::to_string).collect();
        assert_eq!(strings[0], "notice");
        assert!(strings[1].starts_with("1 │"));
        assert!(strings[2].starts_with("   "));
        assert!(strings[3].starts_with("2 │"));
    }

    #[test]
    fn formatted_json_rows_do_not_claim_original_line_numbers() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("data.json");
        fs::write(&path, "{\"a\":1,\"b\":2}").unwrap();
        let app = App::new(temp.path().to_path_buf()).unwrap();
        assert!(!app.preview().document().unwrap().source_lines);
        let cached = viewer_text(&app, 30, &app.theme().palette());
        assert!(cached.source_rows.is_empty());
        assert!(
            crate::document::load_source(&path, &app.theme().palette())
                .unwrap()
                .source_lines
        );
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
    fn editor_preview_uses_examples_and_confines_viewer_to_its_area() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("project")).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let area = Rect::new(9, 4, 60, 12);

        for sample in [
            crate::ThemeSample::Picker,
            crate::ThemeSample::Source,
            crate::ThemeSample::Rendered,
        ] {
            app.set_theme_sample(sample);
            let mut reference = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            reference
                .draw(|frame| render_theme_preview(frame, &app))
                .unwrap();
            let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
            terminal
                .draw(|frame| {
                    let background = vec![Line::raw("x".repeat(80)); 20];
                    frame.render_widget(Paragraph::new(background), frame.area());
                    render_editor_preview(frame, &app, area);
                })
                .unwrap();

            let actual = terminal.backend().buffer();
            let expected = reference.backend().buffer();
            for y in 0..20 {
                for x in 0..80 {
                    if x < area.x || x >= area.right() || y < area.y || y >= area.bottom() {
                        assert_eq!(actual[(x, y)].symbol(), "x");
                    } else if sample != crate::ThemeSample::Picker
                        && y > area.y
                        && y + 1 < area.bottom()
                    {
                        assert_eq!(actual[(x, y)], expected[(x - area.x, y - area.y)]);
                    }
                }
            }
        }
    }

    #[test]
    fn theme_sample_uses_viewer_and_shows_theme_controls() {
        let temp = tempdir().unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.set_theme_sample(crate::ThemeSample::Source);
        let backend = TestBackend::new(88, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_theme_preview(frame, &app))
            .unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("source · theme sample"));
        assert!(rendered.contains("tab sample"));
        assert!(rendered.contains("m mode"));
        assert!(rendered.contains("enter save"));
    }

    #[test]
    fn source_geometry_keeps_original_bytes_through_tabs_clusters_wrapping_and_resize() {
        let temp = tempdir().unwrap();
        let source = "\u{feff}prefix\tCafe\u{301} long words\r\n第二 needle end\r\n";
        fs::write(temp.path().join("notes.txt"), source).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_viewer();
        let target = source.find("needle").unwrap();
        let wide = source_byte_offset(&app, 40, target);
        let narrow = source_byte_offset(&app, 12, target);
        assert!(narrow > wide);
        let cache = viewer_text(&app, 12, &app.theme().palette());
        let (row, cells) = cache
            .geometry
            .iter()
            .enumerate()
            .find(|(_, cells)| cells.iter().any(|cell| cell.bytes.contains(&target)))
            .unwrap();
        let gutter = source_gutter_width(app.preview().document().unwrap().lines.len(), 12);
        let x = cells
            .iter()
            .take_while(|cell| cell.bytes.end <= target)
            .map(|cell| cell.width)
            .sum::<usize>();
        assert_eq!(
            source_hit_test(&app, 12, gutter + x as u16, row),
            Some(target)
        );
        let cluster = cache
            .geometry
            .iter()
            .flatten()
            .find(|cell| cell.text == "e\u{301}")
            .unwrap();
        assert_eq!(&source[cluster.bytes.clone()], "e\u{301}");
        let tab = cache
            .geometry
            .iter()
            .flatten()
            .find(|cell| &source[cell.bytes.clone()] == "\t")
            .unwrap();
        assert_eq!(tab.width, 4);
        for cell in cache.geometry.iter().flatten() {
            assert!(source.is_char_boundary(cell.bytes.start));
            assert!(source.is_char_boundary(cell.bytes.end));
        }
        drop(cache);
        app.begin_viewer_selection(source.find('\t').unwrap());
        app.extend_viewer_selection(target + "needle".len());
        assert_eq!(
            app.selected_viewer_text(),
            Some("\tCafe\u{301} long words\r\n第二 needle")
        );
        let _ = viewer_text(&app, 24, &app.theme().palette());
        assert_eq!(
            app.selected_viewer_text(),
            Some("\tCafe\u{301} long words\r\n第二 needle")
        );
    }

    #[test]
    fn manual_editor_scroll_keeps_caret_and_history_until_keyboard_movement() {
        let temp = tempdir().unwrap();
        fs::write(
            temp.path().join("notes.rs"),
            "let value = 42;\n".repeat(100),
        )
        .unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_editor();
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let area = Rect::new(0, 0, 80, 12);
        let caret = app.editor().unwrap().cursor();
        let revision = app.editor().unwrap().revision();
        let maximum = editor_scrollbar(&app, area).unwrap().maximum;
        set_editor_scroll(&app, maximum, area);
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert_eq!(editor_scrollbar(&app, area).unwrap().offset, maximum);
        assert_eq!(app.editor().unwrap().cursor(), caret);
        assert_eq!(app.editor().unwrap().revision(), revision);
        assert!(!app.editor().unwrap().dirty());
        let rows = editor_page_rows(&app, area);
        app.handle_editor_key(
            termina::event::KeyEvent::new(
                termina::event::KeyCode::Down,
                termina::event::Modifiers::NONE,
            ),
            rows,
        );
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert!(editor_scrollbar(&app, area).unwrap().offset < maximum);
        assert_eq!(app.editor().unwrap().revision(), revision);
    }

    #[test]
    fn clipped_search_matches_keep_graphemes_and_highlight_only_source_text() {
        use crate::search::{SearchHit, SearchHitKind};
        let palette = crate::Theme::auto().palette();
        let source = format!("{}a\u{301}界 tail", "x".repeat(60));
        let hit = SearchHit {
            path: "42.txt".into(),
            is_dir: false,
            is_symlink: false,
            kind: SearchHitKind::Line,
            line: Some(42),
            excerpt: source,
            first_match: 61..63,
            excerpt_start: 0,
        };
        let line = search_result_line(
            "42.txt",
            &hit,
            1,
            14,
            false,
            &palette,
            &crate::matching::LiteralMatcher::new("\u{301}"),
        );
        assert!(
            line.spans
                .iter()
                .any(|span| span.content.contains("a\u{301}")
                    && span.style.add_modifier.contains(Modifier::UNDERLINED))
        );
        assert!(
            line.spans
                .iter()
                .filter(|span| span.content.contains("42"))
                .all(|span| !span.style.add_modifier.contains(Modifier::UNDERLINED))
        );
        assert!(line.width() <= 14);

        let whitespace = SearchHit {
            excerpt: "  visible".into(),
            first_match: 0..2,
            ..hit.clone()
        };
        let line = search_result_line(
            "42.txt",
            &whitespace,
            1,
            18,
            false,
            &palette,
            &crate::matching::LiteralMatcher::new("  "),
        );
        assert!(
            !line.spans[0]
                .style
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
        assert!(
            line.spans.iter().any(|span| span.content == "  "
                && span.style.add_modifier.contains(Modifier::UNDERLINED))
        );

        let query = "needle".repeat(80);
        let clipped = SearchHit {
            excerpt: "needle".repeat(40),
            first_match: 0..query.len(),
            ..hit.clone()
        };
        let line = search_result_line(
            "42.txt",
            &clipped,
            1,
            40,
            false,
            &palette,
            &crate::matching::LiteralMatcher::new(&query),
        );
        assert!(line.spans.iter().any(|span| span.content.contains("needle")
            && span.style.add_modifier.contains(Modifier::UNDERLINED)));

        let name = SearchHit {
            kind: SearchHitKind::Name,
            line: None,
            first_match: 0..6,
            ..hit.clone()
        };
        let line = search_result_line(
            "needle-parent/a-very-long-basename-for-clipping.txt",
            &name,
            0,
            30,
            false,
            &palette,
            &crate::matching::LiteralMatcher::new("needle"),
        );
        assert!(line.spans.iter().any(|span| span.content.contains("needle")
            && span.style.add_modifier.contains(Modifier::UNDERLINED)));
        assert!(line.spans.iter().any(
            |span| span.content.contains("a-very-long") && span.style.fg == Some(palette.text)
        ));
        assert!(line.width() <= 30);

        let header = SearchHit {
            kind: SearchHitKind::File,
            excerpt: String::new(),
            ..hit
        };
        let line = search_result_line(
            "long/directory/42.txt",
            &header,
            2,
            20,
            false,
            &palette,
            &crate::matching::LiteralMatcher::new("42"),
        );
        let text = line.to_string();
        assert!(text.contains("42.txt"));
        assert!(
            line.spans
                .iter()
                .all(|span| !span.style.add_modifier.contains(Modifier::UNDERLINED))
        );
        assert!(
            line.spans
                .iter()
                .any(|span| span.content.contains("42.txt") && span.style.fg == Some(palette.text))
        );
        for width in [112, 80, 40] {
            for (count, suffix) in [(1, "1 line"), (2, "2 lines")] {
                let line = search_result_line(
                    "long/directory/42.txt",
                    &header,
                    count,
                    width,
                    false,
                    &palette,
                    &crate::matching::LiteralMatcher::new("42"),
                );
                assert_eq!(line.width(), width);
                let text = line.to_string();
                assert!(text.ends_with(suffix));
                assert_eq!(text.matches("42.txt").count(), 1);
                assert!(
                    line.spans
                        .iter()
                        .all(|span| { !span.style.add_modifier.contains(Modifier::UNDERLINED) })
                );
            }
        }
    }

    #[test]
    fn search_selection_keeps_effective_contrast_across_matches_gutters_and_padding() {
        use crate::search::{SearchHit, SearchHitKind};
        use ratatui::style::Color;
        let catalog = crate::Config::built_ins();
        let matcher = crate::matching::LiteralMatcher::new("needle");
        let hit = SearchHit {
            path: "needle/needle-file.txt".into(),
            is_dir: false,
            is_symlink: false,
            kind: SearchHitKind::Name,
            line: None,
            excerpt: String::new(),
            first_match: 0..7,
            excerpt_start: 0,
        };
        let fixtures = [
            ("needle/needle-file.txt", hit.clone()),
            (
                "needle/needle-folder/",
                SearchHit {
                    is_dir: true,
                    ..hit.clone()
                },
            ),
            (
                "needle/needle-file.txt",
                SearchHit {
                    kind: SearchHitKind::Line,
                    line: Some(42),
                    excerpt: "prefix needle suffix".into(),
                    first_match: 107..113,
                    excerpt_start: 100,
                    ..hit.clone()
                },
            ),
            (
                "needle/needle-file.txt",
                SearchHit {
                    kind: SearchHitKind::File,
                    ..hit
                },
            ),
        ];
        for theme in ["auto", "dark", "light", "mono"] {
            let palette = catalog.find(theme).unwrap().palette();
            for width in [112, 80, 40, 14] {
                for (name, hit) in &fixtures {
                    let selected =
                        search_result_line(name, hit, 2, width as usize, true, &palette, &matcher);
                    let unselected =
                        search_result_line(name, hit, 2, width as usize, false, &palette, &matcher);
                    assert_eq!(selected.width(), width as usize);
                    let mut terminal = Terminal::new(TestBackend::new(width, 2)).unwrap();
                    terminal
                        .draw(|frame| {
                            frame.render_widget(
                                Paragraph::new(vec![selected.clone(), unselected.clone()]),
                                frame.area(),
                            );
                        })
                        .unwrap();
                    let buffer = terminal.backend().buffer();
                    for (foreground, background) in
                        [(Color::White, Color::Black), (Color::Black, Color::White)]
                    {
                        let effective = |cell: &ratatui::buffer::Cell| {
                            let fg = if cell.fg == Color::Reset {
                                foreground
                            } else {
                                cell.fg
                            };
                            let bg = if cell.bg == Color::Reset {
                                background
                            } else {
                                cell.bg
                            };
                            if cell.modifier.contains(Modifier::REVERSED) {
                                (bg, fg)
                            } else {
                                (fg, bg)
                            }
                        };
                        let colors = effective(&buffer[(0, 0)]);
                        assert_ne!(colors.0, colors.1, "{theme}, width {width}");
                        assert_ne!(colors.1, background, "{theme}, width {width}");
                        for x in 0..width {
                            let cell = &buffer[(x, 0)];
                            assert_eq!(effective(cell), colors, "{theme}, width {width}, x {x}");
                            assert!(!cell.modifier.contains(Modifier::DIM));
                        }
                        assert_ne!(effective(&buffer[(width - 1, 1)]), colors);
                        let matches = (0..width)
                            .filter(|x| buffer[(*x, 0)].modifier.contains(Modifier::UNDERLINED))
                            .collect::<Vec<_>>();
                        if hit.kind == SearchHitKind::File {
                            assert!(matches.is_empty());
                        } else {
                            assert!(!matches.is_empty(), "{theme}, width {width}, {name}");
                            for x in matches {
                                let normal_match = &buffer[(x, 1)];
                                assert!(normal_match.modifier.contains(Modifier::UNDERLINED));
                                assert_eq!(normal_match.fg, palette.accent);
                                assert_ne!(effective(normal_match), colors);
                            }
                        }
                    }
                }
            }
        }
    }
}
