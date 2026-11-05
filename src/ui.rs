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
    render_picker(frame, app, true);
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
    let palette = app.theme().palette();
    let now = SystemTime::now();
    frame.render_widget(Clear, area);

    if app.is_viewing() && !theme_preview {
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

    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

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

fn render_header(frame: &mut Frame<'_>, app: &App, area: Rect, palette: &Palette, badge: &str) {
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

    if app.preview().document().is_some() || app.preview().message().is_some() {
        let (lines, _) =
            preview_text_lines(app, list.width, palette, 0, list.height as usize, false);
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

fn picker_panes(area: Rect) -> [Rect; 3] {
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

fn viewer_body(area: Rect) -> Rect {
    viewer_inner(viewer_outer(area))
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
        .map(|document| {
            if app.is_viewing() && app.is_markdown_source() {
                document.source_lines.as_deref().unwrap_or(&document.lines)
            } else {
                document.lines.as_slice()
            }
        })
        .unwrap_or(&[]);
    wrap_text_lines(
        notices.iter().chain(document_lines.iter()),
        width,
        offset,
        limit,
        count_all,
    )
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
    source: bool,
    rows: Vec<Line<'static>>,
}

fn viewer_text<'a>(app: &'a App, width: u16, palette: &Palette) -> std::cell::Ref<'a, ViewerText> {
    let source = app.is_viewing() && app.is_markdown_source();
    let cache = app.viewer_text_cache();
    let needs_rebuild = cache
        .borrow()
        .as_ref()
        .is_none_or(|text| text.width != width || text.source != source);
    if needs_rebuild {
        let (rows, _) = preview_text_lines(app, width, palette, 0, usize::MAX, true);
        *cache.borrow_mut() = Some(ViewerText {
            width,
            source,
            rows,
        });
    }
    std::cell::Ref::map(cache.borrow(), |cache| cache.as_ref().unwrap())
}

pub fn viewer_scroll_max(app: &App, area: Rect) -> usize {
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
    let body = viewer_inner(outer);
    if body != outer {
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
        .is_some_and(|document| document.source_lines.is_some());
    if area.height >= 3 {
        let [header, _, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(area);
        let heading = if markdown {
            let mode = if app.is_markdown_source() {
                "source"
            } else {
                "rendered"
            };
            format!(" {mode} · {}", app.preview().label())
        } else {
            format!(" view · {}", app.preview().label())
        };
        frame.render_widget(
            Paragraph::new(shorten_right(&heading, header.width as usize)).style(
                Style::default()
                    .fg(palette.title)
                    .add_modifier(Modifier::BOLD),
            ),
            header,
        );
        frame.render_widget(
            Paragraph::new(if markdown && footer.width >= 64 {
                " m source/rendered  q/esc back  ↑↓/jk scroll  pg↑↓ page  o open"
            } else if markdown && footer.width >= 32 {
                " m mode  q/esc back  ↑↓  o open"
            } else if markdown {
                "m mode  q back  ↑↓  o"
            } else if footer.width >= 55 {
                " q/esc back  ↑↓/jk scroll  pg↑↓ page  home/end  o open"
            } else if footer.width >= 30 {
                " q/esc back  ↑↓ scroll  o open"
            } else {
                " q back  ↑↓  o open"
            })
            .style(muted_style(palette)),
            footer,
        );
    }
    let cached = viewer_text(app, body.width, palette);
    let lines = cached
        .rows
        .iter()
        .skip(app.viewer_scroll())
        .take(body.height as usize)
        .cloned()
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), body);
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
    let line = if area.width >= 99 {
        Line::from(vec![
            key(" ↑↓", palette),
            hint(" select  ", palette),
            key("pg↑↓", palette),
            hint(" jump  ", palette),
            key("→", palette),
            hint(" folder  ", palette),
            key("←", palette),
            hint(" parent  ", palette),
            key("/", palette),
            hint(" find  ", palette),
            key("v", palette),
            hint(" view  ", palette),
            key("o", palette),
            hint(" open  ", palette),
            key("↵", palette),
            hint(" go  ", palette),
            key("s", palette),
            hint(format!(" sort:{sort}{direction}  "), palette),
            key(".", palette),
            hint(format!(" hidden:{hidden}"), palette),
        ])
    } else if area.width >= 85 {
        Line::from(vec![
            key(" ↑↓", palette),
            hint(" select  ", palette),
            key("pg↑↓", palette),
            hint(" jump  ", palette),
            key("→", palette),
            hint(" folder  ", palette),
            key("←", palette),
            hint(" parent  ", palette),
            key("/", palette),
            hint(" find  ", palette),
            key("v", palette),
            hint(" view  ", palette),
            key("o", palette),
            hint(" open  ", palette),
            key("↵", palette),
            hint(" go  ", palette),
            key("s", palette),
            hint(format!(" sort:{sort}{direction}"), palette),
        ])
    } else if area.width >= 57 {
        Line::from(vec![
            key(" ↑↓", palette),
            hint("  ", palette),
            key("pg↑↓", palette),
            hint("  ", palette),
            key("→", palette),
            hint("dir  ", palette),
            key("←", palette),
            hint("back  ", palette),
            key("/", palette),
            hint("find  ", palette),
            key("v", palette),
            hint("view  ", palette),
            key("o", palette),
            hint("open  ", palette),
            key("s", palette),
            hint(format!(":{sort}{direction}  "), palette),
            key("↵", palette),
            hint("go", palette),
        ])
    } else if area.width >= 38 {
        Line::from(vec![
            key(" ↑↓", palette),
            hint(" ", palette),
            key("pg↑↓", palette),
            hint(" ", palette),
            key("/", palette),
            hint("find ", palette),
            key("v", palette),
            hint("view ", palette),
            key("o", palette),
            hint("open ", palette),
            key("s", palette),
            hint(format!(":{sort}{direction} "), palette),
            key("↵", palette),
            hint("go", palette),
        ])
    } else if area.width >= 30 {
        Line::from(vec![
            key(" ↑↓", palette),
            hint(" ", palette),
            key("v", palette),
            hint("view ", palette),
            key("o", palette),
            hint("open ", palette),
            key("↵", palette),
            hint("go ", palette),
            key("→", palette),
            hint("dir ", palette),
            key("←", palette),
            hint("back", palette),
        ])
    } else {
        Line::from(vec![
            key(" ↑↓ ", palette),
            key("v", palette),
            hint("view ", palette),
            key("o", palette),
            hint("open ", palette),
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
        assert!(terminal.backend().to_string().contains("three"));
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
        assert_eq!(cache.width, 28);
        assert!(cache.rows.len() < narrow_count);
    }

    #[test]
    fn viewer_border_and_page_rows_follow_terminal_size() {
        let area = Rect::new(2, 3, 40, 12);
        assert_eq!(viewer_body(area), Rect::new(3, 5, 38, 8));
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
        assert!(rendered.contains("Some bold text"));
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
        assert!(rendered.contains("modified"));
        assert!(rendered.contains("main.rs"));
        assert!(rendered.contains('│'));
        assert!(rendered.contains("hidden:off"));
        assert!(rendered.contains("sort:name↑"));
        assert!(rendered.contains("o open"));
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
