use crate::theme_editor::{FIELDS, FieldKind, ThemeEditorDraft};
use crate::{App, ThemeSample};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use termina::event::{
    KeyCode, KeyEvent, Modifiers as KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use unicode_width::UnicodeWidthStr;

/// Result of an editor input event. The caller persists the draft on `Save`.
pub enum EditorAction {
    Continue,
    Save(String),
    Cancel,
}

#[derive(Clone)]
enum Mode {
    Browse,
    Edit {
        text: String,
        cursor: usize,
        initial: String,
        inherited: bool,
    },
    Name {
        text: String,
        cursor: usize,
    },
    Confirm {
        name: String,
    },
}

pub struct ThemeEditorUi {
    pub draft: ThemeEditorDraft,
    selected: usize,
    mode: Mode,
    builtin: bool,
    error: Option<String>,
}

impl ThemeEditorUi {
    pub fn new(draft: ThemeEditorDraft, is_builtin: bool) -> Self {
        Self {
            draft,
            selected: 0,
            mode: Mode::Browse,
            builtin: is_builtin,
            error: None,
        }
    }

    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
    }

    /// The preview uses this exact rectangle for drawing and image placement.
    pub fn preview_area(&self, area: Rect) -> Option<Rect> {
        if area.width < 70 || area.height < 5 {
            return None;
        }
        let left = Self::panel(area);
        Some(Rect::new(
            left.right(),
            area.y,
            area.width - left.width,
            area.height,
        ))
    }

    fn panel(area: Rect) -> Rect {
        let width = if area.width >= 70 && area.height >= 5 {
            (area.width / 2).clamp(35, 54)
        } else {
            area.width
        };
        Rect::new(area.x, area.y, width, area.height)
    }

    // The scroll offset is derived from the selection and current viewport, so
    // resizing cannot leave the selected row outside the panel.
    fn rows(&self, height: usize) -> (Vec<(Option<usize>, &'static str)>, usize) {
        let mut rows = Vec::new();
        let mut group = "";
        let mut selected_row = 0;
        for (index, field) in FIELDS.iter().enumerate() {
            if field.group != group {
                group = field.group;
                rows.push((None, group));
                rows.push((
                    None,
                    match group {
                        "Base" => "  Sample: all samples",
                        "Interface" => "  Sample: Browser",
                        "Syntax" | "Markdown" => "  Sample: Source",
                        "Rendered colors" | "Rendered typography" => "  Sample: Rendered Markdown",
                        _ => "",
                    },
                ));
            }
            if index == self.selected {
                selected_row = rows.len();
            }
            rows.push((Some(index), field.key));
        }
        let offset = selected_row.saturating_add(1).saturating_sub(height);
        (rows, offset)
    }

    pub fn draw(&self, frame: &mut Frame<'_>, app: &App) {
        let area = frame.area();
        if let Some(preview) = self.preview_area(area) {
            crate::ui::render_editor_preview(frame, app, preview);
        }
        let panel = Self::panel(area);
        if panel.width == 0 || panel.height == 0 {
            return;
        }
        frame.render_widget(Clear, panel);
        let palette = app.theme().palette();
        let normal = Style::default().fg(palette.text);
        let muted = Style::default().fg(palette.muted);
        let heading = Style::default()
            .fg(palette.title)
            .add_modifier(Modifier::BOLD);
        let selected = Style::default()
            .fg(palette.emphasis_foreground)
            .bg(palette.emphasis_background);
        if panel.height <= 2 {
            let line = match &self.mode {
                Mode::Name { text, cursor } => {
                    input_line(Line::raw("Name: "), text, *cursor, panel.width, selected)
                }
                Mode::Confirm { name } => Line::styled(format!("Save {name}? Enter/y"), selected),
                Mode::Edit { text, cursor, .. } => input_line(
                    Line::raw(format!("{}: ", FIELDS[self.selected].key)),
                    text,
                    *cursor,
                    panel.width,
                    selected,
                ),
                Mode::Browse => Line::styled(
                    format!(
                        "{}: {}",
                        FIELDS[self.selected].key,
                        self.draft.effective_value(self.selected)
                    ),
                    selected,
                ),
            };
            frame.render_widget(
                Paragraph::new(line),
                Rect::new(panel.x, panel.y, panel.width, 1),
            );
            if panel.height == 2 {
                frame.render_widget(
                    Paragraph::new(
                        self.error
                            .as_deref()
                            .unwrap_or("↵ edit  s save  Tab preview"),
                    )
                    .style(muted),
                    Rect::new(panel.x, panel.y + 1, panel.width, 1),
                );
            }
            return;
        }
        let title = if self.builtin {
            "new custom theme"
        } else {
            self.draft.name()
        };
        frame.render_widget(
            Paragraph::new(format!(
                " theme editor{} · {title}",
                if self.preview_area(area).is_none() {
                    " · preview hidden"
                } else {
                    ""
                }
            ))
            .style(heading),
            Rect::new(panel.x, panel.y, panel.width, 1),
        );
        let footer_y = panel.y + panel.height - 1;
        let content = Rect::new(
            panel.x,
            panel.y + 1,
            panel.width,
            panel.height.saturating_sub(2),
        );
        match &self.mode {
            Mode::Name { text, cursor } => {
                let lines = vec![
                    Line::styled(" Save as a custom theme", heading),
                    input_line(Line::raw(" Name: "), text, *cursor, content.width, normal),
                    Line::styled(" Enter continues · Esc cancels", muted),
                ];
                // Keep the input visible when the content has only one row.
                let lines = if content.height == 1 {
                    vec![lines[1].clone()]
                } else {
                    lines
                };
                frame.render_widget(Paragraph::new(lines), content);
            }
            Mode::Confirm { name } => {
                frame.render_widget(
                    Paragraph::new(vec![
                        Line::styled(" Confirm save", heading),
                        Line::styled(format!(" Save theme {name}?"), normal),
                        Line::styled(" Enter/y save · Esc/n cancel", muted),
                    ]),
                    content,
                );
            }
            _ => {
                let detail_height = content.height.saturating_sub(1).min(4);
                let list = Rect::new(
                    content.x,
                    content.y,
                    content.width,
                    content.height - detail_height,
                );
                let (rows, offset) = self.rows(list.height as usize);
                let lines: Vec<Line<'_>> = rows
                    .into_iter()
                    .skip(offset)
                    .take(list.height as usize)
                    .map(|(index, label)| match index {
                        None if label.starts_with("  ") => Line::styled(label, muted),
                        None => Line::styled(format!(" {label}"), heading),
                        Some(i) => {
                            let value = self.draft.effective_value(i);
                            let prefix = if i == self.selected { "›" } else { " " };
                            let mut spans = vec![Span::raw(format!("{prefix} {label}: "))];
                            if let Some(color) = self.draft.color(i) {
                                spans.push(Span::styled("■ ", Style::default().fg(color)));
                            }
                            if i == self.selected
                                && let Mode::Edit { text, cursor, .. } = &self.mode
                            {
                                return input_line(
                                    Line::from(spans),
                                    text,
                                    *cursor,
                                    list.width,
                                    selected,
                                );
                            }
                            if self.draft.inherited(i) {
                                spans.push(Span::raw("(inherited) "));
                            }
                            spans.push(Span::raw(value));
                            Line::from(spans).style(if i == self.selected {
                                selected
                            } else {
                                normal
                            })
                        }
                    })
                    .collect();
                frame.render_widget(Paragraph::new(lines), list);
                if detail_height > 0 {
                    let detail = Rect::new(content.x, list.bottom(), content.width, detail_height);
                    frame.render_widget(
                        Paragraph::new(format!(
                            "Value: {}\n{}\n{}",
                            self.draft.effective_value(self.selected),
                            if self.draft.inherited(self.selected) {
                                format!("Inherited from {}", self.draft.effective_value(0))
                            } else {
                                "Explicit override".to_owned()
                            },
                            self.draft.guidance(self.selected)
                        ))
                        .style(muted)
                        .wrap(ratatui::widgets::Wrap { trim: true }),
                        detail,
                    );
                }
            }
        }
        let footer = if let Some(error) = &self.error {
            format!(" {error}")
        } else {
            match self.mode {
                Mode::Browse if area.width < 65 => " ↑↓ ↵ edit r reset s save Esc Tab".to_owned(),
                Mode::Browse => {
                    " ↑↓ select  ↵ edit  r reset  s save  Esc cancel  Tab sample  Pg↑↓ scroll"
                        .to_owned()
                }
                Mode::Edit { .. } => {
                    " ↵ accept  Esc cancel  empty = inherit  Tab sample".to_owned()
                }
                Mode::Name { .. } => " Enter continue  Esc cancel".to_owned(),
                Mode::Confirm { .. } => " Enter/y save  Esc/n cancel".to_owned(),
            }
        };
        frame.render_widget(Clear, Rect::new(area.x, footer_y, area.width, 1));
        frame.render_widget(
            Paragraph::new(footer).style(if self.error.is_some() {
                Style::default().fg(palette.error)
            } else {
                muted
            }),
            Rect::new(area.x, footer_y, area.width, 1),
        );
    }

    fn apply(&mut self, app: &mut App, value: &str) -> bool {
        match self.draft.commit(self.selected, value) {
            Ok(()) => {
                app.set_theme(self.draft.preview().clone());
                self.error = None;
                true
            }
            Err(error) => {
                self.error = Some(error.to_string());
                false
            }
        }
    }

    fn start_edit(&mut self) {
        let inherited = self.draft.inherited(self.selected);
        let text = if inherited {
            self.draft.effective_value(self.selected)
        } else {
            self.draft.input(self.selected)
        };
        self.mode = Mode::Edit {
            cursor: text.chars().count(),
            initial: text.clone(),
            inherited,
            text,
        };
    }

    fn save_prompt(&mut self) {
        if self.builtin {
            let text = String::new();
            self.mode = Mode::Name {
                cursor: text.chars().count(),
                text,
            };
        } else {
            self.mode = Mode::Confirm {
                name: self.draft.name().to_owned(),
            };
        }
    }

    fn adjust(&mut self, app: &mut App, increase: bool) {
        let field = &FIELDS[self.selected];
        let value = match field.kind {
            FieldKind::Bool => {
                let palette = self.draft.preview().palette();
                let current = match field.key {
                    "reverse_selection" => palette.reverse_emphasis,
                    "dim_muted" => palette.dim_muted,
                    _ => return,
                };
                (!current).to_string()
            }
            FieldKind::Number => {
                let rendered = self.draft.preview().rendered();
                let current = match field.key {
                    "body_size_px" => rendered.body_size_px,
                    "code_size_px" => rendered.code_size_px,
                    "heading_scale_percent" => rendered.heading_scale_percent,
                    "line_height_percent" => rendered.line_height_percent,
                    "page_padding_px" => rendered.page_padding_px,
                    _ => return,
                };
                (if increase {
                    current.saturating_add(1)
                } else {
                    current.saturating_sub(1)
                })
                .to_string()
            }
            _ => return,
        };
        self.apply(app, &value);
    }

    pub fn handle_key(&mut self, app: &mut App, key: KeyEvent, area: Rect) -> EditorAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return EditorAction::Cancel;
        }
        if key.code == KeyCode::Tab && matches!(self.mode, Mode::Browse | Mode::Edit { .. }) {
            let next = match app.theme_sample() {
                ThemeSample::Picker => ThemeSample::Source,
                ThemeSample::Source => ThemeSample::Rendered,
                ThemeSample::Rendered => ThemeSample::Picker,
            };
            app.set_theme_sample(next);
            return EditorAction::Continue;
        }
        if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
            && app.theme_sample() == ThemeSample::Picker
            && matches!(self.mode, Mode::Browse | Mode::Edit { .. })
        {
            let rows = self
                .preview_area(area)
                .map(crate::ui::browser_sample_page_rows)
                .unwrap_or(1)
                .max(1);
            let maximum = crate::ui::browser_sample_scroll_max(rows);
            let current = app.viewer_scroll().min(maximum);
            let next = if key.code == KeyCode::PageUp {
                current.saturating_sub(rows)
            } else {
                current.saturating_add(rows).min(maximum)
            };
            app.set_viewer_scroll(next);
            return EditorAction::Continue;
        }
        if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) && app.is_viewing() {
            let rows = self
                .preview_area(area)
                .map(crate::ui::viewer_page_rows)
                .unwrap_or(1)
                .max(1);
            let rows = if app.is_rendered_markdown_view() {
                rows.saturating_mul(app.markdown_scroll_step())
            } else {
                rows
            };
            let scroll = if key.code == KeyCode::PageUp {
                app.viewer_scroll().saturating_sub(rows)
            } else {
                app.viewer_scroll().saturating_add(rows)
            };
            app.set_viewer_scroll(scroll);
            return EditorAction::Continue;
        }
        let mode = self.mode.clone();
        match mode {
            Mode::Browse => {
                self.error = None;
                match key.code {
                    KeyCode::Escape | KeyCode::Char('q') => return EditorAction::Cancel,
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.selected = self.selected.saturating_sub(1)
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.selected = (self.selected + 1).min(FIELDS.len().saturating_sub(1))
                    }
                    KeyCode::Left => self.adjust(app, false),
                    KeyCode::Right => self.adjust(app, true),
                    KeyCode::PageUp => {
                        self.selected = self
                            .selected
                            .saturating_sub(usize::from(area.height.saturating_sub(4)).max(1))
                    }
                    KeyCode::PageDown => {
                        self.selected = (self.selected
                            + usize::from(area.height.saturating_sub(4)).max(1))
                        .min(FIELDS.len().saturating_sub(1))
                    }
                    KeyCode::Home => self.selected = 0,
                    KeyCode::End => self.selected = FIELDS.len().saturating_sub(1),
                    KeyCode::Enter | KeyCode::Char('e') => self.start_edit(),
                    KeyCode::Char('c')
                        if matches!(
                            FIELDS[self.selected].kind,
                            FieldKind::Color | FieldKind::RenderedColor
                        ) =>
                    {
                        self.start_edit()
                    }
                    KeyCode::Char('r') | KeyCode::Delete => match self.draft.reset(self.selected) {
                        Ok(()) => app.set_theme(self.draft.preview().clone()),
                        Err(error) => self.error = Some(error.to_string()),
                    },
                    KeyCode::Char('s') => self.save_prompt(),
                    _ => {}
                }
            }
            Mode::Edit {
                mut text,
                mut cursor,
                initial,
                inherited,
            } => {
                match key.code {
                    KeyCode::Escape => {
                        self.error = None;
                        self.mode = Mode::Browse;
                        return EditorAction::Continue;
                    }
                    KeyCode::Enter => {
                        if (inherited && text == initial) || self.apply(app, &text) {
                            self.mode = Mode::Browse;
                        }
                        return EditorAction::Continue;
                    }
                    _ => {}
                }
                if edit_text(&mut text, &mut cursor, key) {
                    self.error = None;
                }
                self.mode = Mode::Edit {
                    text,
                    cursor,
                    initial,
                    inherited,
                };
            }
            Mode::Name {
                mut text,
                mut cursor,
            } => match key.code {
                KeyCode::Escape => {
                    self.mode = Mode::Browse;
                    self.error = None;
                }
                KeyCode::Enter => {
                    if text.trim().is_empty() {
                        self.error = Some("Enter a custom theme name".into());
                    } else {
                        self.error = None;
                        self.mode = Mode::Confirm {
                            name: text.trim().to_owned(),
                        };
                    }
                }
                _ => {
                    edit_text(&mut text, &mut cursor, key);
                    self.mode = Mode::Name { text, cursor };
                }
            },
            Mode::Confirm { name } => match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    return EditorAction::Save(name);
                }
                KeyCode::Escape | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.mode = Mode::Browse;
                    self.error = None;
                }
                _ => {}
            },
        }
        EditorAction::Continue
    }

    /// Mouse coordinates and `area` are relative to the inline viewport.
    pub fn handle_mouse(&mut self, _app: &mut App, mouse: MouseEvent, area: Rect) -> EditorAction {
        let panel = Self::panel(area);
        if mouse.column < panel.x
            || mouse.column >= panel.x.saturating_add(panel.width)
            || mouse.row < panel.y
            || mouse.row >= panel.y.saturating_add(panel.height)
        {
            return EditorAction::Continue;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp if matches!(self.mode, Mode::Browse) => {
                self.selected = self.selected.saturating_sub(1)
            }
            MouseEventKind::ScrollDown if matches!(self.mode, Mode::Browse) => {
                self.selected = (self.selected + 1).min(FIELDS.len().saturating_sub(1))
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
                match self.mode.clone() {
                    Mode::Browse
                        if mouse.row > panel.y
                            && mouse.row
                                < panel.y
                                    + 1
                                    + panel
                                        .height
                                        .saturating_sub(2)
                                        .saturating_sub(panel.height.saturating_sub(3).min(4)) =>
                    {
                        let content_height = panel.height.saturating_sub(2);
                        let (rows, offset) = self.rows(
                            (content_height - content_height.saturating_sub(1).min(4)) as usize,
                        );
                        let row = offset + usize::from(mouse.row - panel.y - 1);
                        if let Some((Some(index), _)) = rows.get(row) {
                            self.selected = *index;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        EditorAction::Continue
    }
}

// The cursor is a character index; clipping is at grapheme boundaries in
// terminal cells. A cursor inside a grapheme marks that whole grapheme.
fn input_line<'a>(
    prefix: Line<'a>,
    text: &str,
    cursor: usize,
    width: u16,
    style: Style,
) -> Line<'a> {
    let width = usize::from(width);
    if width == 0 {
        return Line::default();
    }
    let mut spans = Vec::new();
    let mut used = 0;
    for grapheme in prefix.styled_graphemes(style) {
        let cells = UnicodeWidthStr::width(grapheme.symbol);
        if used + cells > width - 1 {
            break;
        }
        spans.push(Span::styled(grapheme.symbol.to_owned(), grapheme.style));
        used += cells;
    }
    let available = width - used;
    let byte_cursor = text
        .char_indices()
        .nth(cursor)
        .map_or(text.len(), |(i, _)| i);
    let value = Line::raw(text);
    let graphemes: Vec<_> = value.styled_graphemes(style).collect();
    let mut byte_offset = 0;
    let mut caret = graphemes.len();
    for (index, grapheme) in graphemes.iter().enumerate() {
        let end = byte_offset + grapheme.symbol.len();
        if byte_cursor < end {
            caret = index;
            break;
        }
        byte_offset = end;
    }
    let caret_width = graphemes
        .get(caret)
        .map_or(1, |g| UnicodeWidthStr::width(g.symbol).max(1));
    let mut remaining = available.saturating_sub(caret_width.min(available));
    let mut start = caret;
    while start > 0 {
        let cells = UnicodeWidthStr::width(graphemes[start - 1].symbol);
        if cells > remaining {
            break;
        }
        remaining -= cells;
        start -= 1;
    }
    let mut drawn = 0;
    for (index, grapheme) in graphemes.iter().enumerate().skip(start) {
        let cells = UnicodeWidthStr::width(grapheme.symbol);
        if index == caret && cells == 0 {
            spans.push(Span::styled(" ", style.add_modifier(Modifier::REVERSED)));
            drawn += 1;
            continue;
        }
        if cells > available - drawn {
            if index == caret {
                spans.push(Span::styled(" ", style.add_modifier(Modifier::REVERSED)));
            }
            break;
        }
        spans.push(Span::styled(
            grapheme.symbol.to_owned(),
            if index == caret {
                style.add_modifier(Modifier::REVERSED)
            } else {
                grapheme.style
            },
        ));
        drawn += cells;
    }
    if caret == graphemes.len() && drawn < available {
        spans.push(Span::styled(" ", style.add_modifier(Modifier::REVERSED)));
    }
    Line::from(spans).style(style)
}

fn edit_text(text: &mut String, cursor: &mut usize, key: KeyEvent) -> bool {
    let mut chars: Vec<char> = text.chars().collect();
    *cursor = (*cursor).min(chars.len());
    let changed = match key.code {
        KeyCode::Left => {
            *cursor = cursor.saturating_sub(1);
            false
        }
        KeyCode::Right => {
            *cursor = (*cursor + 1).min(chars.len());
            false
        }
        KeyCode::Home => {
            *cursor = 0;
            false
        }
        KeyCode::End => {
            *cursor = chars.len();
            false
        }
        KeyCode::Backspace if *cursor > 0 => {
            *cursor -= 1;
            chars.remove(*cursor);
            true
        }
        KeyCode::Delete if *cursor < chars.len() => {
            chars.remove(*cursor);
            true
        }
        KeyCode::Char(c) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            chars.insert(*cursor, c);
            *cursor += 1;
            true
        }
        _ => false,
    };
    if changed {
        *text = chars.into_iter().collect();
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml_edit::Table;

    fn setup(builtin: bool) -> (tempfile::TempDir, App, ThemeEditorUi) {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(dir.path().to_path_buf()).unwrap();
        let draft = ThemeEditorDraft::new("draft".into(), Table::new()).unwrap();
        app.set_theme(draft.preview().clone());
        (dir, app, ThemeEditorUi::new(draft, builtin))
    }

    fn press(ui: &mut ThemeEditorUi, app: &mut App, code: KeyCode) -> EditorAction {
        ui.handle_key(
            app,
            KeyEvent::new(code, KeyModifiers::NONE),
            Rect::new(0, 0, 80, 20),
        )
    }

    fn caret_symbols(ui: &ThemeEditorUi, app: &App, width: u16, height: u16) -> Vec<String> {
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| ui.draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut symbols = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let cell = &buffer[(x, y)];
                if cell.modifier.contains(Modifier::REVERSED) {
                    symbols.push(cell.symbol().to_owned());
                }
            }
        }
        symbols
    }

    #[test]
    fn value_input_scrolls_to_character_cursor_across_resize() {
        let (_dir, mut app, mut ui) = setup(false);
        ui.selected = FIELDS
            .iter()
            .position(|field| field.key == "body_font")
            .unwrap();
        let path = "/fonts/長い/cafe\u{301}-font.ttf";
        ui.mode = Mode::Edit {
            text: path.to_owned(),
            cursor: path.chars().count(),
            initial: String::new(),
            inherited: false,
        };
        assert_eq!(caret_symbols(&ui, &app, 25, 8), [" "]);
        press(&mut ui, &mut app, KeyCode::Home);
        assert_eq!(caret_symbols(&ui, &app, 25, 8), ["/"]);
        for _ in 0..7 {
            press(&mut ui, &mut app, KeyCode::Right);
        }
        assert_eq!(caret_symbols(&ui, &app, 25, 8), ["長"]);
        assert_eq!(caret_symbols(&ui, &app, 18, 3), ["長"]);
        press(&mut ui, &mut app, KeyCode::End);
        press(&mut ui, &mut app, KeyCode::Left);
        assert_eq!(caret_symbols(&ui, &app, 25, 8), ["f"]);
        if let Mode::Edit { text, .. } = &ui.mode {
            assert_eq!(text, path);
        } else {
            panic!("expected value input");
        }
    }

    #[test]
    fn name_input_edits_and_displays_unicode_caret() {
        let (_dir, mut app, mut ui) = setup(true);
        press(&mut ui, &mut app, KeyCode::Char('s'));
        assert_eq!(caret_symbols(&ui, &app, 20, 8), [" "]);
        for c in "long-名前-e\u{301}-theme".chars() {
            press(&mut ui, &mut app, KeyCode::Char(c));
        }
        assert_eq!(caret_symbols(&ui, &app, 20, 8), [" "]);
        press(&mut ui, &mut app, KeyCode::Home);
        assert_eq!(caret_symbols(&ui, &app, 20, 8), ["l"]);
        for _ in 0..5 {
            press(&mut ui, &mut app, KeyCode::Right);
        }
        assert_eq!(caret_symbols(&ui, &app, 20, 8), ["名"]);
        press(&mut ui, &mut app, KeyCode::Char('新'));
        assert_eq!(caret_symbols(&ui, &app, 20, 8), ["名"]);
        press(&mut ui, &mut app, KeyCode::Backspace);
        assert_eq!(caret_symbols(&ui, &app, 20, 3), ["名"]);
        assert_eq!(caret_symbols(&ui, &app, 1, 1), [" "]);
        if let Mode::Name { text, .. } = &ui.mode {
            assert_eq!(text, "long-名前-e\u{301}-theme");
        } else {
            panic!("expected name input");
        }
    }

    #[test]
    fn combining_mark_cursor_and_zero_width_viewport_are_safe() {
        let line = input_line(Line::raw(""), "e\u{301}x", 1, 4, Style::default());
        assert_eq!(line.spans[0].content, "e\u{301}");
        assert!(
            line.spans[0]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            input_line(Line::raw("label"), "é", 1, 0, Style::default())
                .spans
                .is_empty()
        );
        let line = input_line(Line::raw(""), "\u{301}", 0, 1, Style::default());
        assert_eq!(line.spans[0].content, " ");
        assert!(
            line.spans[0]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn builtin_save_requires_name_and_confirmation() {
        let (_dir, mut app, mut ui) = setup(true);
        press(&mut ui, &mut app, KeyCode::Char('s'));
        press(&mut ui, &mut app, KeyCode::Enter);
        assert!(ui.error.is_some());
        for c in "my-theme".chars() {
            press(&mut ui, &mut app, KeyCode::Char(c));
        }
        assert!(matches!(
            press(&mut ui, &mut app, KeyCode::Enter),
            EditorAction::Continue
        ));
        assert!(
            matches!(press(&mut ui, &mut app, KeyCode::Enter), EditorAction::Save(name) if name == "my-theme")
        );
    }

    #[test]
    fn editing_cancel_and_invalid_values_preserve_draft() {
        let (_dir, mut app, mut ui) = setup(false);
        press(&mut ui, &mut app, KeyCode::Down);
        press(&mut ui, &mut app, KeyCode::Enter);
        for c in "#123456".chars() {
            press(&mut ui, &mut app, KeyCode::Char(c));
        }
        assert_eq!(ui.draft.input(1), "");
        press(&mut ui, &mut app, KeyCode::Escape);
        assert_eq!(ui.draft.input(1), "");
        press(&mut ui, &mut app, KeyCode::Enter);
        for c in "not-a-color".chars() {
            press(&mut ui, &mut app, KeyCode::Char(c));
        }
        press(&mut ui, &mut app, KeyCode::Enter);
        assert!(ui.error.is_some());
        assert_eq!(ui.draft.input(1), "");
        assert!(matches!(ui.mode, Mode::Edit { .. }));
    }

    #[test]
    fn inherited_edit_accept_change_and_reset() {
        let (_dir, mut app, mut ui) = setup(false);
        ui.selected = 1;
        let original = ui.draft.effective_value(ui.selected);
        press(&mut ui, &mut app, KeyCode::Enter);
        press(&mut ui, &mut app, KeyCode::Enter);
        assert!(ui.draft.inherited(ui.selected));
        assert_eq!(ui.draft.effective_value(ui.selected), original);
        press(&mut ui, &mut app, KeyCode::Enter);
        for _ in original.chars() {
            press(&mut ui, &mut app, KeyCode::Backspace);
        }
        for c in "#123456".chars() {
            press(&mut ui, &mut app, KeyCode::Char(c));
        }
        press(&mut ui, &mut app, KeyCode::Enter);
        assert!(!ui.draft.inherited(ui.selected));
        assert_eq!(ui.draft.effective_value(ui.selected), "#123456");
        press(&mut ui, &mut app, KeyCode::Char('r'));
        assert!(ui.draft.inherited(ui.selected));
        assert_eq!(ui.draft.effective_value(ui.selected), original);
    }

    #[test]
    fn arrows_adjust_boolean_and_tab_cycles_without_leaving_editor() {
        let (_dir, mut app, mut ui) = setup(false);
        ui.selected = FIELDS.iter().position(|f| f.key == "dim_muted").unwrap();
        press(&mut ui, &mut app, KeyCode::Right);
        assert!(!ui.draft.input(ui.selected).is_empty());
        press(&mut ui, &mut app, KeyCode::Char('r'));
        assert!(ui.draft.input(ui.selected).is_empty());
        press(&mut ui, &mut app, KeyCode::Enter);
        press(&mut ui, &mut app, KeyCode::Tab);
        assert_eq!(app.theme_sample(), ThemeSample::Source);
        assert!(matches!(ui.mode, Mode::Edit { .. }));
        press(&mut ui, &mut app, KeyCode::Tab);
        assert_eq!(app.theme_sample(), ThemeSample::Rendered);
        press(&mut ui, &mut app, KeyCode::Tab);
        assert_eq!(app.theme_sample(), ThemeSample::Picker);
        press(&mut ui, &mut app, KeyCode::Enter);
        assert!(ui.draft.inherited(ui.selected));
    }

    #[test]
    fn browser_sample_scrolls_without_changing_field_and_survives_theme_edit() {
        let (_dir, mut app, mut ui) = setup(false);
        let area = Rect::new(0, 0, 80, 6);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        ui.handle_key(&mut app, key(KeyCode::PageDown), area);
        let offset = app.viewer_scroll();
        assert!(offset > 0);
        assert_eq!(ui.selected, 0);
        app.set_theme(crate::theme::Config::built_ins().next(app.theme().name()));
        assert_eq!(app.viewer_scroll(), offset);
        ui.handle_key(&mut app, key(KeyCode::PageDown), area);
        ui.handle_key(&mut app, key(KeyCode::PageDown), area);
        ui.handle_key(&mut app, key(KeyCode::PageDown), area);
        let end = app.viewer_scroll();
        assert_eq!(end, crate::ui::browser_sample_scroll_max(2));
        ui.handle_key(&mut app, key(KeyCode::PageUp), area);
        assert!(app.viewer_scroll() < end);
        ui.handle_key(&mut app, key(KeyCode::Tab), area);
        assert_eq!(app.theme_sample(), ThemeSample::Source);
        assert_eq!(app.viewer_scroll(), 0);
    }

    #[test]
    fn preview_layout_keeps_fields_and_hides_when_narrow() {
        let (_dir, _app, ui) = setup(false);
        assert!(ui.preview_area(Rect::new(0, 0, 58, 20)).is_none());
        for width in [88, 140] {
            let area = Rect::new(0, 0, width, 20);
            let preview = ui.preview_area(area).unwrap();
            assert_eq!(ThemeEditorUi::panel(area).right(), preview.x);
            assert_eq!(preview.right(), area.right());
            let body = crate::ui::viewer_body(preview);
            assert!(body.x >= preview.x && body.right() <= preview.right());
            assert!(body.y >= preview.y && body.bottom() <= preview.bottom());
            assert!(body.width > 0 && body.height > 0);
        }
        assert!(ui.preview_area(Rect::new(0, 0, 88, 4)).is_none());
    }
}
