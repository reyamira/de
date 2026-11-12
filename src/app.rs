use crate::{BrowseDefaults, DisplaySettings, Theme};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThemeSample {
    Picker,
    Source,
    Rendered,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub name: OsString,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub modified: Option<SystemTime>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Preview {
    label: String,
    path: Option<PathBuf>,
    entries: Vec<Entry>,
    message: Option<String>,
    document: Option<crate::document::Document>,
    image: Option<crate::media::ImagePreview>,
}

impl Preview {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn document(&self) -> Option<&crate::document::Document> {
        self.document.as_ref()
    }

    pub fn image(&self) -> Option<&crate::media::ImagePreview> {
        self.image.as_ref()
    }
}

impl Entry {
    pub fn display_name(&self) -> String {
        let mut name = self.name.to_string_lossy().into_owned();
        if self.is_dir {
            name.push('/');
        }
        if self.is_symlink {
            name.push('@');
        }
        name
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NavigationResult {
    Continue,
    Accept(PathBuf),
    Open(PathBuf),
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortMode {
    Name,
    Modified,
}

impl SortMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Modified => "time",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Name => Self::Modified,
            Self::Modified => Self::Name,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

impl SortDirection {
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Ascending => "↑",
            Self::Descending => "↓",
        }
    }

    const fn opposite(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }
}

#[derive(Debug)]
struct SearchSnapshot {
    current_dir: PathBuf,
    entries: Vec<Entry>,
    all_entries: Vec<Entry>,
    selected: usize,
}

#[derive(Clone, Debug)]
struct SearchSelection {
    path: PathBuf,
    kind: crate::search::SearchHitKind,
    line: Option<usize>,
}

impl From<&crate::search::SearchHit> for SearchSelection {
    fn from(hit: &crate::search::SearchHit) -> Self {
        Self {
            path: hit.path.clone(),
            kind: hit.kind,
            line: hit.line,
        }
    }
}

#[derive(Debug, Default)]
struct ViewerFind {
    query: String,
    matches: Arc<[std::ops::Range<usize>]>,
    active: Option<usize>,
    prompt: bool,
    saved: Option<(bool, bool, usize, crate::document::Document)>,
    preview_document: Option<crate::document::Document>,
}

#[derive(Debug)]
pub struct App {
    current_dir: PathBuf,
    all_entries: Vec<Entry>,
    entries: Vec<Entry>,
    selected: usize,
    show_hidden: bool,
    status: Option<String>,
    preview: Preview,
    search_query: String,
    search_scope: crate::search::SearchScope,
    search_snapshot: Option<SearchSnapshot>,
    search_worker: crate::search::SearchWorker,
    search_generation: Option<u64>,
    search_hits: Vec<crate::search::SearchHit>,
    search_selection: Option<SearchSelection>,
    search_name_count: usize,
    search_line_count: usize,
    search_file_lines: std::collections::HashMap<PathBuf, usize>,
    search_scanned: usize,
    search_truncated: bool,
    viewer_source_line: Option<usize>,
    editor: Option<crate::editor::Editor>,
    editor_cache: RefCell<Option<crate::ui::EditorText>>,
    sort_mode: SortMode,
    sort_direction: SortDirection,
    theme: Theme,
    display: DisplaySettings,
    viewing: bool,
    viewer_expanded: bool,
    viewer_scroll: usize,
    markdown_source: bool,
    reading: bool,
    image_loader: Option<crate::image_loader::ImageLoader>,
    image_request: Option<u64>,
    viewer_text_cache: RefCell<Option<crate::ui::ViewerText>>,
    markdown_renderer: Option<crate::markdown_renderer::MarkdownRenderer>,
    markdown_request: Option<u64>,
    markdown_key: Option<(u32, u32, u32, Arc<crate::render_theme::RenderedTheme>)>,
    markdown_layout: Option<(u32, u32, Arc<crate::render_theme::RenderedTheme>)>,
    markdown_page: Option<crate::markdown_renderer::RenderedPage>,
    markdown_error: Option<String>,
    markdown_cell_height: usize,
    theme_sample: ThemeSample,
    find: ViewerFind,
    viewer_selection: Option<(usize, usize)>,
    reveal_match: bool,
    browser_target: Option<std::ops::Range<usize>>,
}

impl App {
    pub fn new(start: PathBuf) -> io::Result<Self> {
        Self::with_defaults(start, BrowseDefaults::default())
    }

    pub fn with_defaults(start: PathBuf, defaults: BrowseDefaults) -> io::Result<Self> {
        if !start.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("not a directory: {}", start.display()),
            ));
        }

        let BrowseDefaults {
            show_hidden,
            sort_mode,
            sort_direction,
        } = defaults;
        let entries = read_entries(&start, show_hidden, sort_mode, sort_direction)?;
        let mut app = Self {
            current_dir: start,
            all_entries: entries.clone(),
            entries,
            selected: 0,
            show_hidden,
            status: None,
            preview: Preview {
                label: "preview".into(),
                path: None,
                entries: Vec::new(),
                message: None,
                document: None,
                image: None,
            },
            search_query: String::new(),
            search_scope: crate::search::SearchScope::default(),
            search_snapshot: None,
            search_worker: crate::search::SearchWorker::new(),
            search_generation: None,
            search_hits: Vec::new(),
            search_selection: None,
            search_name_count: 0,
            search_line_count: 0,
            search_file_lines: std::collections::HashMap::new(),
            search_scanned: 0,
            search_truncated: false,
            viewer_source_line: None,
            editor: None,
            editor_cache: RefCell::new(None),
            sort_mode,
            sort_direction,
            theme: Theme::auto(),
            display: DisplaySettings::default(),
            viewing: false,
            viewer_expanded: false,
            viewer_scroll: 0,
            markdown_source: false,
            reading: false,
            image_loader: None,
            image_request: None,
            viewer_text_cache: RefCell::new(None),
            markdown_renderer: None,
            markdown_request: None,
            markdown_key: None,
            markdown_layout: None,
            markdown_page: None,
            markdown_error: None,
            markdown_cell_height: 16,
            theme_sample: ThemeSample::Picker,
            find: ViewerFind::default(),
            viewer_selection: None,
            reveal_match: false,
            browser_target: None,
        };
        app.refresh_preview();
        Ok(app)
    }

    pub fn current_dir(&self) -> &Path {
        &self.current_dir
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn total_entry_count(&self) -> usize {
        self.all_entries.len()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = Some(status.into());
    }

    /// The current in-session toggles, in the form saved to `[defaults]`.
    pub fn browse_defaults(&self) -> BrowseDefaults {
        BrowseDefaults {
            show_hidden: self.show_hidden,
            sort_mode: self.sort_mode,
            sort_direction: self.sort_direction,
        }
    }

    pub fn preview(&self) -> &Preview {
        &self.preview
    }

    pub fn is_viewing(&self) -> bool {
        self.viewing
    }

    pub fn viewer_expanded(&self) -> bool {
        self.viewing && self.viewer_expanded
    }

    pub fn toggle_viewer_expanded(&mut self) {
        if self.viewing && self.editor.is_none() && !self.find.prompt {
            self.viewer_expanded = !self.viewer_expanded;
            self.invalidate_markdown_layout();
        }
    }

    pub fn theme_sample(&self) -> ThemeSample {
        self.theme_sample
    }

    pub fn set_theme_sample(&mut self, sample: ThemeSample) {
        self.theme_sample = sample;
        self.cancel_image_request();
        self.cancel_markdown();
        self.viewer_text_cache.get_mut().take();
        self.viewer_scroll = 0;
        self.viewer_expanded = false;
        self.reading = false;
        self.status = None;
        if sample == ThemeSample::Picker {
            self.viewing = false;
            self.preview.path = None;
            self.refresh_preview();
        } else {
            self.viewing = true;
            self.markdown_source = sample == ThemeSample::Source;
            self.preview = Preview {
                label: "theme sample".into(),
                path: Some(self.current_dir.join(".de-theme-sample.md")),
                entries: Vec::new(),
                message: None,
                document: Some(crate::document::markdown_sample(&self.theme.palette())),
                image: None,
            };
        }
    }

    pub fn is_markdown_source(&self) -> bool {
        self.markdown_source
    }

    pub fn is_reading(&self) -> bool {
        self.viewing && self.reading
    }

    pub fn supports_reading(&self) -> bool {
        self.preview
            .document
            .as_ref()
            .is_some_and(|document| document.supports_reading)
    }

    pub fn toggle_reading(&mut self) {
        if !self.viewing {
            return;
        }
        if self.supports_reading() {
            self.reading = !self.reading;
            self.viewer_scroll = 0;
            self.status = None;
            self.viewer_selection = None;
            self.update_find_matches();
        } else {
            self.status = Some("reading mode is available for plain text only".into());
        }
    }

    pub(crate) fn viewer_text_cache(&self) -> &RefCell<Option<crate::ui::ViewerText>> {
        &self.viewer_text_cache
    }

    pub fn toggle_markdown_source(&mut self) {
        if self.viewing
            && self
                .preview
                .document
                .as_ref()
                .is_some_and(|document| document.markdown_source.is_some())
        {
            self.browser_target = None;
            self.markdown_source = !self.markdown_source;
            self.reading = false;
            self.status = None;
            self.viewer_scroll = 0;
            if self.theme_sample != ThemeSample::Picker {
                self.theme_sample = if self.markdown_source {
                    ThemeSample::Source
                } else {
                    ThemeSample::Rendered
                };
            }
            self.cancel_markdown();
            self.viewer_selection = None;
            self.update_find_matches();
        }
    }

    /// Apply only the image prepared for the current selection and refresh.
    pub fn poll_preview(&mut self) -> bool {
        let Some((request, result)) = self.image_loader.as_mut().and_then(|loader| loader.poll())
        else {
            return false;
        };
        if self.image_request != Some(request) {
            return false;
        }
        self.image_request = None;
        self.viewer_text_cache.get_mut().take();
        self.preview.message = None;
        match result {
            Ok(image) => self.preview.image = Some(image),
            Err(error) => self.preview.message = Some(error),
        }
        true
    }

    pub fn image_pending(&self) -> bool {
        self.image_loader
            .as_ref()
            .is_some_and(|loader| loader.is_pending())
    }

    pub fn is_rendered_markdown_view(&self) -> bool {
        self.viewing
            && self.editor.is_none()
            && !self.markdown_source
            && self
                .preview
                .document
                .as_ref()
                .is_some_and(|d| d.markdown_source.is_some())
    }

    pub fn markdown_image(&self) -> Option<&crate::media::ImagePreview> {
        self.is_rendered_markdown_view()
            .then_some(self.markdown_page.as_ref())
            .flatten()
            .map(|page| &page.image)
    }

    pub fn markdown_message(&self) -> Option<&str> {
        self.markdown_error.as_deref().or_else(|| {
            self.markdown_page
                .is_none()
                .then_some("Rendering Markdown with FMD…")
        })
    }

    pub fn markdown_pending(&self) -> bool {
        self.markdown_renderer
            .as_ref()
            .is_some_and(|renderer| renderer.is_pending())
    }

    pub fn markdown_scroll_step(&self) -> usize {
        self.markdown_cell_height
    }

    pub fn markdown_scroll_max(&self) -> usize {
        self.markdown_page
            .as_ref()
            .map_or(usize::MAX, |page| page.max_scroll as usize)
    }

    pub fn update_markdown_view(&mut self, width: u32, height: u32, cell_height: u32) {
        if !self.is_rendered_markdown_view() || width == 0 || height == 0 {
            return;
        }
        self.markdown_cell_height = cell_height.max(1) as usize;
        let scroll = self.viewer_scroll.min(u32::MAX as usize) as u32;
        let style = self.theme.rendered();
        let unchanged = self.markdown_key.as_ref().is_some_and(|old| {
            old.0 == width && old.1 == height && old.2 == scroll && Arc::ptr_eq(&old.3, &style)
        });
        if unchanged {
            return;
        }
        if self
            .markdown_layout
            .as_ref()
            .is_some_and(|old| old.0 != width || old.1 != height || !Arc::ptr_eq(&old.2, &style))
        {
            self.markdown_page = None;
        }
        let Some(path) = self.preview.path.clone() else {
            return;
        };
        let Some(source) = self
            .preview
            .document
            .as_ref()
            .and_then(|d| d.markdown_source.clone())
        else {
            return;
        };
        let annotations = self.text_annotations();
        let renderer = self
            .markdown_renderer
            .get_or_insert_with(crate::markdown_renderer::MarkdownRenderer::new);
        self.markdown_request = Some(renderer.request(crate::markdown_renderer::RenderRequest {
            path,
            source,
            width,
            height,
            scroll_y: scroll,
            style: Arc::clone(&style),
            annotations,
        }));
        self.markdown_layout = Some((width, height, Arc::clone(&style)));
        self.markdown_key = Some((width, height, scroll, style));
        self.markdown_error = None;
    }

    pub fn poll_markdown(&mut self) -> bool {
        let Some((generation, result)) = self
            .markdown_renderer
            .as_mut()
            .and_then(|renderer| renderer.poll())
        else {
            return false;
        };
        if self.markdown_request != Some(generation) || !self.is_rendered_markdown_view() {
            return false;
        }
        self.markdown_request = None;
        self.apply_markdown_result(result);
        true
    }

    fn apply_markdown_result(
        &mut self,
        result: Result<crate::markdown_renderer::RenderedPage, String>,
    ) {
        match result {
            Ok(page) => {
                let layout_changed = self
                    .markdown_page
                    .as_ref()
                    .is_none_or(|old| !Arc::ptr_eq(&old.text, &page.text));
                self.viewer_scroll = page.scroll_y as usize;
                if let Some(key) = &mut self.markdown_key {
                    key.2 = page.scroll_y;
                }
                self.markdown_error = None;
                self.markdown_page = Some(page);
                if layout_changed {
                    self.update_find_matches();
                }
                if layout_changed && self.browser_target.is_some() && self.active_match().is_none()
                {
                    // Only genuinely hidden Markdown syntax opens in source mode.
                    self.markdown_source = true;
                    self.viewer_text_cache.get_mut().take();
                    self.update_find_matches();
                    self.reveal_match = true;
                }
            }
            Err(error) => {
                self.markdown_page = None;
                self.markdown_error = Some(error);
            }
        }
    }

    fn cancel_markdown(&mut self) {
        if let Some(renderer) = self.markdown_renderer.as_mut() {
            renderer.cancel();
        }
        self.markdown_request = None;
        self.markdown_key = None;
        self.markdown_layout = None;
        self.markdown_page = None;
        self.markdown_error = None;
    }

    pub fn viewer_scroll(&self) -> usize {
        self.viewer_scroll
    }

    pub fn open_viewer(&mut self) {
        match self.selected_entry() {
            Some(entry) if !entry.is_dir => {
                self.viewing = true;
                self.viewer_expanded = false;
                self.viewer_scroll = 0;
                self.find = ViewerFind::default();
                self.viewer_selection = None;
                self.browser_target = self.selected_search_hit().and_then(|hit| {
                    let line = hit.line?;
                    let range = hit.first_match.clone();
                    let source = &self.preview.document.as_ref()?.source;
                    let start = source_line_start(source, line)?;
                    Some(start + range.start..start + range.end)
                });
                self.viewer_source_line = self.selected_search_hit().and_then(|hit| hit.line);
                self.markdown_source = false;
                self.reading = false;
                if self.is_searching() {
                    self.find.query = self.search_query.clone();
                    self.update_find_matches();
                    self.reveal_match = self.browser_target.is_some();
                }
                self.status = None;
            }
            _ => self.status = Some("select a file to view".into()),
        }
    }

    pub fn close_viewer(&mut self) {
        let original = self.find.preview_document.take().or_else(|| {
            self.find
                .saved
                .take()
                .map(|(_, _, _, document)| document)
                .filter(|document| !document.source_lines)
        });
        if let Some(document) = original {
            self.preview.document = Some(document);
            self.viewer_text_cache.get_mut().take();
        }
        self.viewing = false;
        self.viewer_expanded = false;
        self.viewer_scroll = 0;
        self.reading = false;
        self.status = None;
        self.find = ViewerFind::default();
        self.viewer_selection = None;
        self.browser_target = None;
        self.cancel_markdown();
    }

    pub fn set_viewer_scroll(&mut self, scroll: usize) {
        if self.is_rendered_markdown_view() && self.viewer_scroll != scroll {
            if let Some(renderer) = self.markdown_renderer.as_mut() {
                renderer.cancel();
            }
            self.markdown_request = None;
            self.markdown_key = None;
        }
        self.viewer_scroll = scroll;
    }

    pub fn is_searching(&self) -> bool {
        self.search_snapshot.is_some()
    }

    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    pub fn search_scope(&self) -> crate::search::SearchScope {
        self.search_scope
    }

    pub fn cycle_search_scope(&mut self) {
        self.search_scope = self.search_scope.next();
        if self.is_searching() {
            self.request_search();
        }
    }

    pub fn search_counts(&self) -> (usize, usize, usize) {
        (
            self.search_name_count,
            self.search_line_count,
            self.search_file_lines.len(),
        )
    }

    pub fn search_file_line_count(&self, index: usize) -> usize {
        self.search_hit(index)
            .filter(|hit| hit.kind != crate::search::SearchHitKind::Line)
            .and_then(|hit| self.search_file_lines.get(&hit.path).copied())
            .unwrap_or(0)
    }

    pub fn search_pending(&self) -> bool {
        self.search_worker.is_pending()
    }

    /// Wake the inline loop only for workers whose results it can consume.
    pub fn background_work_pending(&self) -> bool {
        self.image_pending()
            || self.markdown_pending()
            || (!self.viewing && self.editor.is_none() && self.search_pending())
    }

    pub fn search_scanned(&self) -> usize {
        self.search_scanned
    }

    pub fn search_truncated(&self) -> bool {
        self.search_truncated
    }

    pub fn search_hit(&self, index: usize) -> Option<&crate::search::SearchHit> {
        self.search_hits.get(index)
    }

    fn selected_search_hit(&self) -> Option<&crate::search::SearchHit> {
        self.search_hit(self.selected)
    }

    pub fn editor(&self) -> Option<&crate::editor::Editor> {
        self.editor.as_ref()
    }

    pub(crate) fn editor_cache(&self) -> &RefCell<Option<crate::ui::EditorText>> {
        &self.editor_cache
    }

    pub fn open_editor(&mut self) {
        let Some(entry) = self.selected_entry().filter(|entry| !entry.is_dir) else {
            self.set_status("Select a regular text file to edit.");
            return;
        };
        let path = entry.path.clone();
        let line = self
            .selected_search_hit()
            .and_then(|hit| hit.line)
            .unwrap_or_else(|| {
                if self.viewing && !self.is_rendered_markdown_view() {
                    crate::ui::viewer_source_at_scroll(self).unwrap_or(1)
                } else {
                    1
                }
            });
        match crate::editor::Editor::open(path, line) {
            Ok(editor) => {
                self.cancel_image_request();
                self.cancel_markdown();
                self.viewer_selection = None;
                self.editor = Some(editor);
                self.editor_cache.get_mut().take();
                self.status = None;
            }
            Err(error) => self.set_status(error),
        }
    }

    pub fn handle_editor_key(&mut self, key: termina::event::KeyEvent, rows: usize) {
        crate::ui::resume_editor_cursor(self);
        if let Some(editor) = &mut self.editor
            && matches!(
                editor.handle_key(key, rows),
                crate::editor::EditorOutcome::Close
            )
        {
            self.editor = None;
            self.editor_cache.get_mut().take();
            let scroll = self.viewer_scroll;
            let source = self.markdown_source;
            let reading = self.reading;
            self.preview.path = None;
            self.find.preview_document = None;
            self.refresh_preview();
            self.viewer_scroll = scroll;
            self.markdown_source = source;
            self.reading = reading;
            self.browser_target = None;
            self.update_find_matches();
        }
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(editor) = &mut self.editor {
            editor.paste(text);
        } else if self.find.prompt {
            self.find
                .query
                .extend(text.chars().filter(|ch| !ch.is_control()));
            self.update_find_matches();
            self.reveal_match = true;
            self.status = None;
        } else if self.is_searching() && !self.viewing {
            self.search_query
                .extend(text.chars().filter(|ch| !ch.is_control()));
            self.request_search();
        }
    }

    pub fn reveal_viewer_source(&mut self, area: ratatui::layout::Rect) {
        if self.is_rendered_markdown_view() {
            if self.reveal_match
                && let (Some(page), Some(range)) =
                    (self.markdown_page.as_ref(), self.active_match())
                && let Some(rect) = page.text.rectangles(&range).first()
            {
                self.set_viewer_scroll((rect.y.max(0.0) as usize).min(self.markdown_scroll_max()));
                self.reveal_match = false;
                self.viewer_source_line = None;
            }
        } else if self.reveal_match {
            if let Some(range) = self.active_match() {
                self.viewer_scroll = crate::ui::source_byte_offset(
                    self,
                    crate::ui::viewer_body(area).width,
                    range.start,
                );
            }
            self.reveal_match = false;
            self.viewer_source_line = None;
        } else if let Some(line) = self.viewer_source_line.take() {
            self.viewer_scroll =
                crate::ui::source_line_offset(self, crate::ui::viewer_body(area).width, line);
        }
    }

    pub fn resize_viewer_interaction(&mut self) {
        self.reveal_match = self.active_match().is_some();
        self.invalidate_markdown_layout();
    }

    fn invalidate_markdown_layout(&mut self) {
        // No coordinates from an old native layout may survive a resize.
        self.markdown_page = None;
        self.markdown_key = None;
        self.markdown_layout = None;
        if let Some(renderer) = self.markdown_renderer.as_mut() {
            renderer.cancel();
        }
        self.markdown_request = None;
    }

    pub fn find_prompt(&self) -> bool {
        self.find.prompt
    }

    pub fn begin_find(&mut self) {
        if self.preview.document.is_none() {
            return;
        }
        self.invalidate_annotations();
        self.find.saved = self
            .preview
            .document
            .clone()
            .map(|doc| (self.markdown_source, self.reading, self.viewer_scroll, doc));
        self.find.prompt = true;
        self.viewer_selection = None;
        self.find.query.clear();
        self.browser_target = None;
        self.status = None;
        // Formatted JSON has no original-byte row geometry. Find uses its raw source.
        if self
            .preview
            .document
            .as_ref()
            .is_some_and(|doc| !doc.source_lines)
            && let Some(path) = self.preview.path.as_ref()
        {
            let source = &self.preview.document.as_ref().unwrap().source;
            self.preview.document = Some(crate::document::source_from_text(
                path,
                source,
                &self.theme.palette(),
            ));
        }
        self.reading = false;
        self.viewer_text_cache.get_mut().take();
        self.update_find_matches();
    }

    pub fn push_find_char(&mut self, ch: char) {
        self.find.query.push(ch);
        self.update_find_matches();
        self.reveal_match = true;
        self.status = None;
    }

    pub fn pop_find_char(&mut self) {
        self.find.query.pop();
        self.update_find_matches();
        self.reveal_match = true;
        self.status = None;
    }

    pub fn finish_find(&mut self, cancel: bool) {
        self.find.prompt = false;
        if cancel {
            if let Some((source, reading, scroll, document)) = self.find.saved.take() {
                self.markdown_source = source;
                self.reading = reading;
                self.preview.document = Some(document);
                self.set_viewer_scroll(scroll);
                self.viewer_text_cache.get_mut().take();
            }
            self.find.query.clear();
            self.find.matches = Arc::from([]);
            self.find.active = None;
            self.reveal_match = false;
            self.invalidate_annotations();
        } else if let Some((_, _, _, document)) = self.find.saved.take()
            && !document.source_lines
        {
            self.find.preview_document.get_or_insert(document);
        }
    }

    fn update_find_matches(&mut self) {
        let matcher = crate::matching::LiteralMatcher::new(&self.find.query);
        let text = if self.is_rendered_markdown_view() {
            self.markdown_page.as_ref().map(|page| page.text.text())
        } else {
            self.preview.document.as_ref().map(|doc| {
                let end =
                    source_line_start(&doc.source, doc.lines.len() + 1).unwrap_or(doc.source.len());
                &doc.source[..end]
            })
        };
        let ranges = text.map_or_else(Vec::new, |text| matcher.ranges(text));
        let previous = self.active_match();
        let active = self
            .browser_target
            .as_ref()
            .and_then(|source| {
                if self.is_rendered_markdown_view() {
                    self.markdown_page
                        .as_ref()?
                        .text
                        .source_match(source.clone(), &ranges)
                } else {
                    Some(source.clone())
                }
            })
            .or_else(|| {
                self.browser_target
                    .is_none()
                    .then(|| previous.clone())
                    .flatten()
            });
        self.find.active = active
            .as_ref()
            .and_then(|range| ranges.iter().position(|candidate| candidate == range))
            .or_else(|| (self.browser_target.is_none() && !ranges.is_empty()).then_some(0));
        let changed = self.find.matches.as_ref() != ranges.as_slice();
        self.find.matches = ranges.into();
        if changed || previous != self.active_match() {
            self.reveal_match = true;
            self.invalidate_annotations();
        }
    }

    pub fn next_match(&mut self, backwards: bool) {
        let count = self.find.matches.len();
        if count == 0 {
            return;
        }
        let index = self.find.active.unwrap_or(0);
        self.find.active = Some(if backwards {
            (index + count - 1) % count
        } else {
            (index + 1) % count
        });
        self.browser_target = None;
        self.reveal_match = true;
        self.status = None;
        self.invalidate_annotations();
    }

    pub(crate) fn active_match(&self) -> Option<std::ops::Range<usize>> {
        self.find
            .active
            .and_then(|index| self.find.matches.get(index))
            .cloned()
    }

    pub(crate) fn viewer_matches(&self) -> &[std::ops::Range<usize>] {
        &self.find.matches
    }

    pub fn find_status(&self) -> Option<String> {
        if self.find.query.is_empty() && !self.find.prompt {
            return None;
        }
        Some(format!(
            " / {}{}  {} / {}{}  n/N next  Esc {}",
            self.find.query,
            if self.find.prompt { "▏" } else { "" },
            self.find.active.map_or(0, |index| index + 1),
            self.find.matches.len(),
            if self.find.matches.is_empty() {
                " · no matches"
            } else {
                ""
            },
            if self.find.prompt { "restore" } else { "back" }
        ))
    }

    pub(crate) fn selection_range(&self) -> Option<std::ops::Range<usize>> {
        self.viewer_selection
            .and_then(|(a, b)| (a != b).then_some(a.min(b)..a.max(b)))
    }

    pub fn begin_viewer_selection(&mut self, byte: usize) {
        self.viewer_selection = Some((byte, byte));
        self.invalidate_annotations();
    }

    pub fn extend_viewer_selection(&mut self, byte: usize) {
        if let Some((_, end)) = &mut self.viewer_selection {
            *end = byte;
        }
        self.invalidate_annotations();
    }

    pub fn has_viewer_selection(&self) -> bool {
        self.selection_range().is_some()
    }

    pub fn selected_viewer_text(&self) -> Option<&str> {
        let range = self.selection_range()?;
        let text = if self.is_rendered_markdown_view() {
            self.markdown_page.as_ref()?.text.text()
        } else {
            &self.preview.document.as_ref()?.source
        };
        text.get(range)
    }

    pub fn copy_viewer_selection(&mut self, writer: &mut impl io::Write) -> io::Result<()> {
        use base64::Engine as _;
        const LIMIT: usize = 100_000;
        let Some(text) = self.selected_viewer_text() else {
            self.set_status("Select text before copying.");
            return Ok(());
        };
        if text.len() > LIMIT {
            self.set_status("Selection exceeds the 100000-byte clipboard limit.");
            return Ok(());
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(text);
        write!(writer, "\x1b]52;c;{encoded}\x07")?;
        writer.flush()?;
        self.set_status("Selection sent to terminal clipboard.");
        Ok(())
    }

    pub fn viewer_hit_test(
        &self,
        area: ratatui::layout::Rect,
        column: u16,
        row: u16,
        cell_width: u32,
        cell_height: u32,
    ) -> Option<usize> {
        let body = crate::ui::viewer_body(area);
        if body.is_empty() {
            return None;
        }
        let x = column.clamp(body.x, body.right() - 1) - body.x;
        let y = row.clamp(body.y, body.bottom() - 1) - body.y;
        if self.is_rendered_markdown_view() {
            let layout = self.markdown_layout.as_ref()?;
            if layout.0 != u32::from(body.width) * cell_width
                || layout.1 != u32::from(body.height) * cell_height
            {
                return None;
            }
            let page = self.markdown_page.as_ref()?;
            page.text.hit_test(
                (x as f32 + 0.5) * cell_width as f32,
                (y as f32 + 0.5) * cell_height as f32 + page.scroll_y as f32,
            )
        } else {
            crate::ui::source_hit_test(self, body.width, x, self.viewer_scroll + y as usize)
        }
    }

    fn text_annotations(&self) -> crate::rendered_text::TextAnnotations {
        crate::rendered_text::TextAnnotations {
            selection: self.selection_range(),
            matches: Arc::clone(&self.find.matches),
            active: self.active_match(),
        }
    }

    fn invalidate_annotations(&mut self) {
        self.markdown_key = None;
    }

    pub fn sort_mode(&self) -> SortMode {
        self.sort_mode
    }

    pub fn sort_direction(&self) -> SortDirection {
        self.sort_direction
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn set_theme(&mut self, theme: Theme) {
        let sample_scroll = self.viewer_scroll;
        let changed = self.theme.name() != theme.name()
            || self.theme.palette() != theme.palette()
            || !Arc::ptr_eq(&self.theme.rendered(), &theme.rendered());
        if changed && self.preview.document.is_some() {
            self.preview.path = None;
        }
        self.theme = theme;
        if changed {
            self.cancel_markdown();
            self.viewer_text_cache.get_mut().take();
            self.viewer_scroll = 0;
        }
        if self.theme_sample != ThemeSample::Picker {
            self.set_theme_sample(self.theme_sample);
            self.viewer_scroll = sample_scroll;
        } else {
            self.refresh_preview();
            if !self.viewing {
                self.viewer_scroll = sample_scroll;
            }
        }
    }

    pub fn display_settings(&self) -> &DisplaySettings {
        &self.display
    }

    pub fn set_display_settings(&mut self, display: DisplaySettings) {
        self.display = display;
    }

    pub fn move_up(&mut self) {
        self.status = None;
        self.search_selection = None;
        self.selected = self.selected.saturating_sub(1);
        self.refresh_preview();
    }

    pub fn move_down(&mut self) {
        self.status = None;
        self.search_selection = None;
        if !self.entries.is_empty() {
            self.selected = (self.selected + 1).min(self.entries.len() - 1);
        }
        self.refresh_preview();
    }

    pub fn move_first(&mut self) {
        self.status = None;
        self.search_selection = None;
        self.selected = 0;
        self.refresh_preview();
    }

    pub fn move_last(&mut self) {
        self.status = None;
        self.search_selection = None;
        self.selected = self.entries.len().saturating_sub(1);
        self.refresh_preview();
    }

    pub fn page_up(&mut self, rows: usize) {
        self.status = None;
        self.search_selection = None;
        self.selected = self.selected.saturating_sub(rows.max(1));
        self.refresh_preview();
    }

    pub fn page_down(&mut self, rows: usize) {
        self.status = None;
        self.search_selection = None;
        if !self.entries.is_empty() {
            self.selected = self
                .selected
                .saturating_add(rows.max(1))
                .min(self.entries.len() - 1);
        }
        self.refresh_preview();
    }

    pub fn begin_search(&mut self) {
        if self.is_searching() {
            return;
        }
        self.search_snapshot = Some(SearchSnapshot {
            current_dir: self.current_dir.clone(),
            entries: self.entries.clone(),
            all_entries: self.all_entries.clone(),
            selected: self.selected,
        });
        self.search_query.clear();
        self.request_search();
    }

    pub fn push_search_char(&mut self, character: char) {
        self.search_query.push(character);
        self.request_search();
    }

    pub fn pop_search_char(&mut self) {
        self.search_query.pop();
        self.request_search();
    }

    pub fn cancel_search(&mut self) {
        self.search_worker.cancel();
        self.search_generation = None;
        self.search_hits.clear();
        self.search_selection = None;
        self.search_name_count = 0;
        self.search_line_count = 0;
        self.search_file_lines.clear();
        self.search_query.clear();
        if let Some(snapshot) = self.search_snapshot.take() {
            self.current_dir = snapshot.current_dir;
            self.entries = snapshot.entries;
            self.all_entries = snapshot.all_entries;
            self.selected = snapshot.selected;
            self.status = None;
            self.refresh_preview();
        }
    }

    fn request_search(&mut self) {
        if self.search_selection.is_none() {
            self.search_selection = self.selected_search_hit().map(SearchSelection::from);
        }
        self.search_generation = Some(self.search_worker.request(
            self.current_dir.clone(),
            self.search_query.clone(),
            self.show_hidden,
            self.search_scope,
        ));
        self.search_hits.clear();
        self.search_name_count = 0;
        self.search_line_count = 0;
        self.search_file_lines.clear();
        self.entries.clear();
        self.selected = 0;
        self.search_scanned = 0;
        self.search_truncated = false;
        self.status = None;
        self.refresh_preview();
    }

    pub fn poll_search(&mut self) -> bool {
        if self.viewing || self.editor.is_some() {
            return false;
        }
        let mut changed = false;
        let mut done = false;
        let mut selected = None;
        for _ in 0..4 {
            let Some(batch) = self.search_worker.poll() else {
                break;
            };
            if Some(batch.generation) != self.search_generation || !self.is_searching() {
                continue;
            }
            if !changed && self.search_selection.is_none() {
                selected = self.selected_search_hit().map(SearchSelection::from);
            }
            self.search_scanned = batch.scanned;
            self.search_truncated |= batch.truncated;
            done |= batch.done;
            self.merge_search_hits(batch.hits);
            changed = true;
        }
        if changed {
            self.restore_search_selection(selected.as_ref(), done);
            self.refresh_preview();
        }
        changed
    }

    fn merge_search_hits(&mut self, hits: Vec<crate::search::SearchHit>) {
        use crate::search::SearchHitKind;
        for hit in hits {
            let position = self.search_hits.binary_search_by(|existing| {
                existing
                    .path
                    .cmp(&hit.path)
                    .then(existing.kind.cmp(&hit.kind))
                    .then(existing.line.cmp(&hit.line))
            });
            let Err(index) = position else {
                continue;
            };
            match hit.kind {
                SearchHitKind::Name => self.search_name_count += 1,
                SearchHitKind::Line => {
                    self.search_line_count += 1;
                    if let Some(count) = self.search_file_lines.get_mut(&hit.path) {
                        *count += 1;
                    } else {
                        self.search_file_lines.insert(hit.path.clone(), 1);
                    }
                }
                SearchHitKind::File => {}
            }
            self.entries.insert(
                index,
                Entry {
                    name: hit
                        .path
                        .strip_prefix(&self.current_dir)
                        .unwrap_or(&hit.path)
                        .as_os_str()
                        .to_owned(),
                    path: hit.path.clone(),
                    is_dir: hit.is_dir,
                    is_symlink: hit.is_symlink,
                    modified: None,
                },
            );
            self.search_hits.insert(index, hit);
        }
    }

    fn restore_search_selection(&mut self, selected: Option<&SearchSelection>, done: bool) {
        use crate::search::{SearchHitKind, SearchScope};
        let Some(anchor) = self.search_selection.as_ref().or(selected) else {
            self.selected = 0;
            return;
        };
        let start = self
            .search_hits
            .partition_point(|hit| hit.path < anchor.path);
        let end = start + self.search_hits[start..].partition_point(|hit| hit.path == anchor.path);
        let group = &self.search_hits[start..end];
        let desired = group.iter().position(|hit| match self.search_scope {
            SearchScope::Names => hit.kind == SearchHitKind::Name,
            SearchScope::Content if anchor.kind == SearchHitKind::Name => {
                hit.kind == SearchHitKind::Line
            }
            _ if anchor.kind == SearchHitKind::Line => {
                hit.kind == SearchHitKind::Line && hit.line == anchor.line
            }
            _ => hit.kind != SearchHitKind::Line,
        });
        if let Some(index) = desired {
            self.selected = start + index;
            self.search_selection = None;
        } else {
            // Keep a same-file header visible while the exact line is still in flight.
            self.selected = if group.is_empty() { 0 } else { start };
            if done {
                self.search_selection = None;
            }
        }
    }

    pub fn cycle_sort(&mut self) {
        let selected_name = self.selected_entry().map(|entry| entry.name.clone());
        self.sort_mode = self.sort_mode.next();
        sort_entries(&mut self.all_entries, self.sort_mode, self.sort_direction);
        self.apply_sort(selected_name.as_deref());
    }

    pub fn toggle_sort_direction(&mut self) {
        let selected_name = self.selected_entry().map(|entry| entry.name.clone());
        self.sort_direction = self.sort_direction.opposite();
        sort_entries(&mut self.all_entries, self.sort_mode, self.sort_direction);
        self.apply_sort(selected_name.as_deref());
    }

    pub fn enter_selected(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        if !entry.is_dir {
            self.open_viewer();
            return;
        }

        let target = entry.path.clone();
        self.navigate_to(target, None);
    }

    pub fn go_parent(&mut self) {
        let Some(parent) = self.current_dir.parent() else {
            self.status = Some("already at the filesystem root".into());
            return;
        };
        if parent == self.current_dir {
            self.status = Some("already at the filesystem root".into());
            return;
        }

        let child_name = self.current_dir.file_name().map(OsStr::to_os_string);
        self.navigate_to(parent.to_path_buf(), child_name.as_deref());
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.reload_preserving_selection();
    }

    pub fn refresh(&mut self) {
        self.preview.path = None;
        self.reload_preserving_selection();
    }

    pub fn accept_current(&self) -> NavigationResult {
        NavigationResult::Accept(self.current_dir.clone())
    }

    pub fn activate_selected(&mut self) -> NavigationResult {
        match self.selected_entry() {
            Some(entry) if entry.is_dir => NavigationResult::Accept(entry.path.clone()),
            Some(_) => {
                self.open_viewer();
                NavigationResult::Continue
            }
            None => {
                self.set_status("Nothing selected.");
                NavigationResult::Continue
            }
        }
    }

    pub fn open_selected(&mut self) -> NavigationResult {
        let Some(entry) = self.selected_entry() else {
            self.status = Some("nothing selected to open".into());
            return NavigationResult::Continue;
        };
        if entry.is_dir {
            self.status = Some("select a file to open; use right to enter directories".into());
            return NavigationResult::Continue;
        }

        NavigationResult::Open(entry.path.clone())
    }

    fn navigate_to(&mut self, target: PathBuf, select_name: Option<&OsStr>) {
        match read_entries(
            &target,
            self.show_hidden,
            self.sort_mode,
            self.sort_direction,
        ) {
            Ok(entries) => {
                self.current_dir = target;
                self.search_worker.cancel();
                self.search_generation = None;
                self.search_hits.clear();
                self.search_selection = None;
                self.search_name_count = 0;
                self.search_line_count = 0;
                self.search_file_lines.clear();
                self.all_entries = entries;
                self.entries = self.all_entries.clone();
                self.selected = select_name
                    .and_then(|name| self.entries.iter().position(|entry| entry.name == name))
                    .unwrap_or(0);
                self.status = None;
                self.refresh_preview();
                if self.is_searching() {
                    self.request_search();
                }
            }
            Err(error) => {
                self.status = Some(format!("cannot open {}: {error}", target.display()));
            }
        }
    }

    fn reload_preserving_selection(&mut self) {
        let selected_name = self.selected_entry().map(|entry| entry.name.clone());
        match read_entries(
            &self.current_dir,
            self.show_hidden,
            self.sort_mode,
            self.sort_direction,
        ) {
            Ok(entries) => {
                self.all_entries = entries;
                self.apply_sort(selected_name.as_deref());
                self.status = None;
            }
            Err(error) => {
                self.status = Some(format!(
                    "cannot refresh {}: {error}",
                    self.current_dir.display()
                ));
            }
        }
    }

    fn apply_sort(&mut self, selected_name: Option<&OsStr>) {
        self.entries = self.all_entries.clone();
        self.selected = selected_name
            .and_then(|name| self.entries.iter().position(|entry| entry.name == name))
            .unwrap_or(0);
        self.status = None;
        self.refresh_preview();
    }

    fn refresh_preview(&mut self) {
        let Some(entry) = self.selected_entry() else {
            self.cancel_image_request();
            self.cancel_markdown();
            self.viewer_text_cache.get_mut().take();
            self.markdown_source = false;
            self.reading = false;
            self.viewer_scroll = 0;
            self.preview = Preview {
                label: "preview".into(),
                path: None,
                entries: Vec::new(),
                message: Some("nothing selected".into()),
                document: None,
                image: None,
            };
            return;
        };

        let label = entry.display_name();
        let path = entry.path.clone();
        let is_dir = entry.is_dir;
        let source_line = self.selected_search_hit().and_then(|hit| hit.line);
        if !is_dir
            && self.preview.path.as_deref() == Some(path.as_path())
            && (source_line.is_none()
                || self
                    .preview
                    .document
                    .as_ref()
                    .is_some_and(|doc| doc.source_lines))
        {
            return;
        }
        self.cancel_image_request();
        self.cancel_markdown();
        self.viewer_text_cache.get_mut().take();
        self.markdown_source = false;
        self.reading = false;
        self.viewer_scroll = 0;
        let mut preview = Preview {
            label,
            path: Some(path.clone()),
            entries: Vec::new(),
            message: None,
            document: None,
            image: None,
        };
        if is_dir {
            match read_entries(&path, self.show_hidden, self.sort_mode, self.sort_direction) {
                Ok(entries) => {
                    preview.message = entries.is_empty().then(|| "empty directory".into());
                    preview.entries = entries;
                }
                Err(error) => preview.message = Some(format!("cannot preview: {error}")),
            }
        } else if crate::media::is_image_path(&path) {
            let loader = self
                .image_loader
                .get_or_insert_with(crate::image_loader::ImageLoader::new);
            self.image_request = Some(loader.request(path.clone()));
            preview.message = Some("loading image…".into());
        } else if path.extension().and_then(OsStr::to_str).is_some_and(|ext| {
            [
                "mp4", "mkv", "webm", "mov", "avi", "m4v", "mpeg", "mpg", "wmv",
            ]
            .iter()
            .any(|video| ext.eq_ignore_ascii_case(video))
        }) {
            preview.message = Some("video playback is external · press o to open".into());
        } else {
            let loaded = if source_line.is_some() {
                crate::document::load_source(&path, &self.theme.palette())
            } else {
                crate::document::load(&path, &self.theme.palette())
            };
            match loaded {
                Ok(document) => preview.document = Some(document),
                Err(error) => {
                    preview.message = Some(format!("{error} · press o to open externally"))
                }
            }
        }
        self.preview = preview;
    }

    fn cancel_image_request(&mut self) {
        if let Some(loader) = self.image_loader.as_mut() {
            loader.cancel();
        }
        self.image_request = None;
    }
}

fn source_line_start(source: &str, line: usize) -> Option<usize> {
    crate::document::logical_lines(source)
        .nth(line.checked_sub(1)?)
        .map(|text| text.as_ptr() as usize - source.as_ptr() as usize)
}

fn read_entries(
    path: &Path,
    show_hidden: bool,
    sort_mode: SortMode,
    sort_direction: SortDirection,
) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for result in fs::read_dir(path)? {
        let dir_entry = match result {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let name = dir_entry.file_name();
        if !show_hidden && is_hidden(&name) {
            continue;
        }

        let file_type = match dir_entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        let metadata = dir_entry.metadata().ok();
        let is_symlink = file_type.is_symlink();
        let is_dir = file_type.is_dir()
            || (is_symlink && metadata.as_ref().is_some_and(|metadata| metadata.is_dir()));
        let modified = metadata.and_then(|metadata| metadata.modified().ok());

        entries.push(Entry {
            name,
            path: dir_entry.path(),
            is_dir,
            is_symlink,
            modified,
        });
    }

    sort_entries(&mut entries, sort_mode, sort_direction);
    Ok(entries)
}

fn sort_entries(entries: &mut [Entry], sort_mode: SortMode, sort_direction: SortDirection) {
    entries.sort_by(|left, right| compare_entries(left, right, sort_mode, sort_direction));
}

fn compare_entries(
    left: &Entry,
    right: &Entry,
    sort_mode: SortMode,
    sort_direction: SortDirection,
) -> Ordering {
    right
        .is_dir
        .cmp(&left.is_dir)
        .then_with(|| match sort_mode {
            SortMode::Name => apply_direction(compare_names(left, right), sort_direction),
            SortMode::Modified => match (left.modified, right.modified) {
                (Some(left_time), Some(right_time)) => apply_direction(
                    left_time
                        .cmp(&right_time)
                        .then_with(|| compare_names(left, right)),
                    sort_direction,
                ),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => apply_direction(compare_names(left, right), sort_direction),
            },
        })
}

fn apply_direction(order: Ordering, sort_direction: SortDirection) -> Ordering {
    match sort_direction {
        SortDirection::Ascending => order,
        SortDirection::Descending => order.reverse(),
    }
}

fn compare_names(left: &Entry, right: &Entry) -> Ordering {
    left.name
        .to_string_lossy()
        .to_lowercase()
        .cmp(&right.name.to_string_lossy().to_lowercase())
        .then_with(|| left.name.cmp(&right.name))
}

fn is_hidden(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::time::Duration;
    use tempfile::tempdir;

    fn fixture() -> (tempfile::TempDir, App) {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("zeta")).unwrap();
        fs::create_dir(temp.path().join("Alpha")).unwrap();
        File::create(temp.path().join("Alpha/inside.txt")).unwrap();
        fs::create_dir(temp.path().join(".secret")).unwrap();
        File::create(temp.path().join("notes.txt")).unwrap();
        let app = App::new(temp.path().to_path_buf()).unwrap();
        (temp, app)
    }

    fn apply_rendered_markdown(app: &mut App, width: u32) {
        let request = crate::markdown_renderer::RenderRequest {
            path: app.preview.path.clone().unwrap(),
            source: app
                .preview
                .document
                .as_ref()
                .unwrap()
                .markdown_source
                .clone()
                .unwrap(),
            width,
            height: 160,
            scroll_y: app.viewer_scroll as u32,
            style: app.theme.rendered(),
            annotations: app.text_annotations(),
        };
        let page = crate::markdown_renderer::render_test_page(&request).unwrap();
        app.apply_markdown_result(Ok(page));
    }

    #[test]
    fn viewer_expansion_invalidates_native_geometry_without_revealing_an_old_match() {
        let temp = tempdir().unwrap();
        let source = format!(
            "# Title\n\nneedle\n\n{}",
            "A paragraph of text.\n\n".repeat(80)
        );
        fs::write(temp.path().join("notes.md"), source).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_viewer();
        apply_rendered_markdown(&mut app, 640);
        app.begin_find();
        for ch in "needle".chars() {
            app.push_find_char(ch);
        }
        app.finish_find(false);
        app.reveal_viewer_source(ratatui::layout::Rect::new(0, 0, 83, 14));
        app.set_viewer_scroll(96);
        apply_rendered_markdown(&mut app, 640);
        assert_eq!(app.viewer_scroll(), 96);
        assert!(app.markdown_image().is_some());
        app.toggle_viewer_expanded();
        assert!(app.viewer_expanded());
        assert!(app.markdown_image().is_none());
        assert!(app.markdown_page.is_none());
        assert!(app.markdown_key.is_none());
        assert!(app.markdown_layout.is_none());
        assert!(app.markdown_request.is_none());
        app.reveal_viewer_source(ratatui::layout::Rect::new(0, 0, 83, 48));
        assert_eq!(app.viewer_scroll(), 96);
        assert_eq!(app.find.query, "needle");
        apply_rendered_markdown(&mut app, 640);
        assert_eq!(app.viewer_scroll(), 96);
        assert!(app.markdown_image().is_some());
        app.toggle_viewer_expanded();
        assert!(!app.viewer_expanded());
        assert_eq!(app.viewer_scroll(), 96);
        assert!(app.markdown_page.is_none());
    }

    #[test]
    fn explicit_rendered_mode_survives_a_hidden_browser_hit_after_rerender() {
        let temp = tempdir().unwrap();
        let source = "[visible label](https://hidden.example/path) and hidden.example\n";
        fs::write(temp.path().join("notes.md"), source).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_search();
        for ch in "hidden.example".chars() {
            app.push_search_char(ch);
        }
        finish_search(&mut app);
        app.open_viewer();
        let start = source.find("hidden.example").unwrap();
        let target = start..start + "hidden.example".len();
        assert_eq!(app.browser_target, Some(target.clone()));
        assert!(app.is_rendered_markdown_view());
        assert!(app.markdown_page.is_none());
        assert!(app.active_match().is_none());

        // A later visible occurrence must not replace the hidden browser target.
        apply_rendered_markdown(&mut app, 320);
        assert!(app.is_markdown_source());
        assert_eq!(app.active_match(), Some(target));
        app.toggle_markdown_source();
        assert!(app.is_rendered_markdown_view());
        assert!(app.browser_target.is_none());
        apply_rendered_markdown(&mut app, 320);
        assert!(app.is_rendered_markdown_view());
        assert!(app.markdown_image().is_some());
        assert!(app.active_match().is_some());
        app.resize_viewer_interaction();
        apply_rendered_markdown(&mut app, 240);
        assert!(app.is_rendered_markdown_view());
        assert!(app.markdown_image().is_some());
    }

    #[test]
    fn visible_browser_hit_keeps_its_identity_until_markdown_layout_and_after_reflow() {
        let temp = tempdir().unwrap();
        let source = "needle elsewhere\n\n[needle](https://hidden.invalid) and needle\n";
        fs::write(temp.path().join("notes.md"), source).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_search();
        for ch in "needle".chars() {
            app.push_search_char(ch);
        }
        finish_search(&mut app);
        app.move_down();
        app.move_down();
        assert_eq!(app.selected_search_hit().unwrap().line, Some(3));
        app.open_viewer();
        let start = source.find("[needle]").unwrap() + 1;
        let target = start..start + "needle".len();
        assert_eq!(app.browser_target, Some(target.clone()));
        assert!(app.is_rendered_markdown_view());
        assert!(app.markdown_page.is_none());
        assert!(app.active_match().is_none());

        for width in [320, 240] {
            apply_rendered_markdown(&mut app, width);
            assert!(app.is_rendered_markdown_view());
            assert_eq!(app.browser_target, Some(target.clone()));
            assert_eq!(app.viewer_matches().len(), 3);
            assert_eq!(app.active_match(), Some(app.viewer_matches()[1].clone()));
            app.resize_viewer_interaction();
        }
    }

    #[test]
    fn accepted_json_find_restores_formatted_snapshot_on_close_and_reopen() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("data.json");
        let raw = "{\"nested\":{\"text\":\"needle\"}}";
        fs::write(&path, raw).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let formatted = app.preview.document.clone().unwrap();
        app.open_viewer();
        fs::write(&path, "{\"changed\":true}").unwrap();
        app.begin_find();
        for ch in "needle".chars() {
            app.push_find_char(ch);
        }
        app.finish_find(false);
        assert!(app.preview.document.as_ref().unwrap().source_lines);
        assert_eq!(app.preview.document.as_ref().unwrap().source.as_ref(), raw);
        app.begin_find();
        for ch in "nested".chars() {
            app.push_find_char(ch);
        }
        app.finish_find(true);
        assert!(app.preview.document.as_ref().unwrap().source_lines);
        app.close_viewer();
        assert_eq!(app.preview.document.as_ref(), Some(&formatted));
        app.open_viewer();
        assert_eq!(app.preview.document.as_ref(), Some(&formatted));
        assert!(!app.preview.document.as_ref().unwrap().source_lines);
    }

    #[test]
    fn browser_activation_reveals_the_matching_wrapped_row_beyond_the_excerpt_prefix() {
        use ratatui::{Terminal, backend::TestBackend};
        let temp = tempdir().unwrap();
        let source = format!("{}NEEDLE tail\n", "padding ".repeat(80));
        fs::write(temp.path().join("notes.txt"), &source).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_search();
        for ch in "needle".chars() {
            app.push_search_char(ch);
        }
        finish_search(&mut app);
        assert_eq!(
            app.selected_search_hit().unwrap().kind,
            crate::search::SearchHitKind::File
        );
        assert!(app.selected_search_hit().unwrap().first_match.start > 240);
        assert!(app.search_hit(1).unwrap().excerpt_start > 240);
        let entries = app.entries.clone();
        app.open_viewer();
        assert_eq!(&source[app.active_match().unwrap()], "NEEDLE");
        app.reveal_viewer_source(ratatui::layout::Rect::new(0, 0, 30, 8));
        assert!(app.viewer_scroll() > 0);
        let mut terminal = Terminal::new(TestBackend::new(30, 8)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &app))
            .unwrap();
        assert!(terminal.backend().to_string().contains("NEEDLE"));
        app.close_viewer();
        assert_eq!(app.entries, entries);
        assert_eq!(app.search_query(), "needle");
    }

    #[test]
    fn local_find_navigates_source_bytes_and_restores_view_without_touching_browser_results() {
        let temp = tempdir().unwrap();
        let source = format!("first copper\n{}\nlast COPPER İ\n", "padding ".repeat(80));
        fs::write(temp.path().join("notes.txt"), &source).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_search();
        for ch in "copper".chars() {
            app.push_search_char(ch);
        }
        finish_search(&mut app);
        let hits = app.search_hits.clone();
        let selected = app.selected;
        app.open_viewer();
        app.toggle_reading();
        app.set_viewer_scroll(3);
        app.begin_find();
        for ch in "copper".chars() {
            app.push_find_char(ch);
        }
        assert_eq!(app.viewer_matches().len(), 2);
        let first = app.active_match().unwrap();
        app.finish_find(false);
        app.next_match(false);
        let second = app.active_match().unwrap();
        assert!(second.start > first.start);
        assert_eq!(&source[second], "COPPER");
        app.next_match(false);
        assert_eq!(app.active_match(), Some(first));
        app.begin_find();
        for ch in "missing".chars() {
            app.push_find_char(ch);
        }
        assert!(app.find_status().unwrap().contains("no matches"));
        app.finish_find(true);
        assert_eq!(app.search_hits, hits);
        assert_eq!(app.selected, selected);
        assert_eq!(app.search_query(), "copper");
        app.close_viewer();
        assert_eq!(app.search_hits, hits);
    }

    #[test]
    fn find_cancel_restores_reading_scroll_and_formatted_json() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("notes.txt"), "one\ntwo\nneedle\n").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_viewer();
        app.toggle_reading();
        app.set_viewer_scroll(7);
        app.begin_find();
        for ch in "needle".chars() {
            app.push_find_char(ch);
        }
        app.reveal_viewer_source(ratatui::layout::Rect::new(0, 0, 16, 5));
        assert!(!app.is_reading());
        app.finish_find(true);
        assert!(app.is_reading());
        assert_eq!(app.viewer_scroll(), 7);

        let temp = tempdir().unwrap();
        let raw = "{\"duplicate\":\"needle\",\"nested\":{\"text\":\"NEEDLE\"}}";
        fs::write(temp.path().join("data.json"), raw).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let formatted = app.preview.document.clone().unwrap();
        app.open_viewer();
        fs::write(temp.path().join("data.json"), "{\"changed\":true}").unwrap();
        app.begin_find();
        for ch in "needle".chars() {
            app.push_find_char(ch);
        }
        assert!(app.preview.document.as_ref().unwrap().source_lines);
        assert_eq!(&raw[app.active_match().unwrap()], "needle");
        app.finish_find(true);
        assert_eq!(app.preview.document.as_ref(), Some(&formatted));
    }

    #[test]
    fn viewer_copy_preserves_selected_bytes_and_only_writes_osc52_to_supplied_writer() {
        use base64::Engine as _;
        let temp = tempdir().unwrap();
        let source = "\u{feff}first\tCafe\u{301}\r\n第二 line\r\n";
        fs::write(temp.path().join("notes.txt"), source).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_viewer();
        let start = source.find('\t').unwrap();
        let end = source.find(" line").unwrap();
        app.begin_viewer_selection(end);
        app.extend_viewer_selection(start);
        let expected = &source[start..end];
        assert_eq!(app.selected_viewer_text(), Some(expected));
        let mut output = Vec::new();
        app.copy_viewer_selection(&mut output).unwrap();
        assert_eq!(
            output,
            format!(
                "\x1b]52;c;{}\x07",
                base64::engine::general_purpose::STANDARD.encode(expected)
            )
            .as_bytes()
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("notes.txt")).unwrap(),
            source
        );
        assert_eq!(app.status(), Some("Selection sent to terminal clipboard."));
    }

    #[test]
    fn sample_reuses_real_viewer_and_invalidates_render_on_theme_change() {
        let temp = tempdir().unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let catalog = crate::Config::built_ins();
        app.set_theme_sample(ThemeSample::Rendered);
        assert!(app.is_rendered_markdown_view());
        let source = app
            .preview()
            .document()
            .unwrap()
            .markdown_source
            .as_ref()
            .unwrap();
        assert!(
            source.contains('#')
                && source.contains("](")
                && source.contains("```")
                && source.contains('|')
        );
        assert!(!temp.path().join(".de-theme-sample.md").exists());
        app.update_markdown_view(640, 480, 16);
        let first = app.markdown_request.unwrap();
        app.update_markdown_view(640, 480, 16);
        assert_eq!(app.markdown_request, Some(first));
        app.set_viewer_scroll(20);
        app.set_theme(catalog.find("light").unwrap().clone());
        assert_eq!(app.viewer_scroll(), 20);
        assert!(app.markdown_key.is_none());
        app.update_markdown_view(640, 480, 16);
        assert_ne!(app.markdown_request, Some(first));
        assert!(Arc::ptr_eq(
            &app.markdown_key.as_ref().unwrap().3,
            &app.theme().rendered()
        ));
        app.set_theme_sample(ThemeSample::Picker);
        assert!(!app.is_viewing());
        assert!(app.preview().document().is_none());
    }

    #[test]
    fn markdown_page_is_discarded_when_mode_or_selection_changes() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("a.md"), "# A").unwrap();
        fs::write(temp.path().join("b.md"), "# B").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_viewer();
        app.markdown_page = Some(crate::markdown_renderer::RenderedPage {
            image: crate::media::ImagePreview {
                width: 10,
                height: 20,
                encoded: "image".into(),
            },
            scroll_y: 0,
            max_scroll: 250,
            text: Arc::new(crate::rendered_text::RenderedText::default()),
        });
        assert_eq!(app.markdown_scroll_max(), 250);
        assert!(app.markdown_image().is_some());
        app.toggle_markdown_source();
        assert!(app.markdown_image().is_none());
        app.toggle_markdown_source();
        assert!(app.markdown_image().is_none());
        app.move_down();
        assert!(app.markdown_image().is_none());
        assert_eq!(app.markdown_scroll_max(), usize::MAX);
    }

    #[test]
    fn lists_directories_first_and_hides_dotfiles() {
        let (_temp, app) = fixture();
        let names: Vec<_> = app
            .entries()
            .iter()
            .map(|entry| entry.display_name())
            .collect();
        assert_eq!(names, ["Alpha/", "zeta/", "notes.txt"]);
        assert!(app.entries().iter().all(|entry| entry.modified.is_some()));
    }

    #[test]
    fn starts_from_configured_defaults() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("Alpha")).unwrap();
        fs::create_dir(temp.path().join("zeta")).unwrap();
        fs::create_dir(temp.path().join(".secret")).unwrap();
        let defaults = BrowseDefaults {
            show_hidden: true,
            sort_mode: SortMode::Name,
            sort_direction: SortDirection::Descending,
        };

        let app = App::with_defaults(temp.path().to_path_buf(), defaults).unwrap();

        let names: Vec<_> = app
            .entries()
            .iter()
            .map(|entry| entry.display_name())
            .collect();
        assert_eq!(names, ["zeta/", "Alpha/", ".secret/"]);
        assert_eq!(app.browse_defaults(), defaults);
    }

    #[test]
    fn toggling_hidden_preserves_selection() {
        let (_temp, mut app) = fixture();
        app.move_down();
        assert_eq!(app.selected_entry().unwrap().display_name(), "zeta/");

        app.toggle_hidden();

        assert!(app.show_hidden());
        assert_eq!(app.selected_entry().unwrap().display_name(), "zeta/");
        assert!(app.entries().iter().any(|entry| entry.name == ".secret"));
    }

    #[test]
    fn enters_a_directory_and_parent_selects_the_child() {
        let (temp, mut app) = fixture();
        app.enter_selected();
        assert_eq!(app.current_dir(), temp.path().join("Alpha"));

        app.go_parent();

        assert_eq!(app.current_dir(), temp.path());
        assert_eq!(app.selected_entry().unwrap().display_name(), "Alpha/");
    }

    #[test]
    fn selecting_a_file_does_not_navigate() {
        let (temp, mut app) = fixture();
        app.move_last();
        app.enter_selected();
        assert_eq!(app.current_dir(), temp.path());
        assert!(app.is_viewing());
    }

    #[test]
    fn current_folder_accept_and_selected_folder_activation_are_distinct() {
        let (temp, mut app) = fixture();
        assert_eq!(
            app.accept_current(),
            NavigationResult::Accept(temp.path().to_path_buf())
        );
        assert_eq!(
            app.activate_selected(),
            NavigationResult::Accept(temp.path().join("Alpha"))
        );
    }

    #[test]
    fn open_returns_the_highlighted_file_without_changing_directory() {
        let (temp, mut app) = fixture();
        app.move_last();

        assert_eq!(
            app.open_selected(),
            NavigationResult::Open(temp.path().join("notes.txt"))
        );
        assert_eq!(app.current_dir(), temp.path());
    }

    #[test]
    fn open_rejects_a_highlighted_directory_without_navigating() {
        let (temp, mut app) = fixture();

        assert_eq!(app.open_selected(), NavigationResult::Continue);
        assert_eq!(app.current_dir(), temp.path());
        assert!(app.status().unwrap().contains("select a file"));
    }

    #[test]
    fn preview_follows_the_highlight_without_navigating() {
        let (temp, mut app) = fixture();
        assert_eq!(app.current_dir(), temp.path());
        assert_eq!(app.preview().label(), "Alpha/");
        assert_eq!(app.preview().entries()[0].display_name(), "inside.txt");

        app.move_down();

        assert_eq!(app.current_dir(), temp.path());
        assert_eq!(app.preview().label(), "zeta/");
        assert_eq!(app.preview().message(), Some("empty directory"));

        app.move_last();
        assert_eq!(app.preview().label(), "notes.txt");
        assert!(app.preview().document().is_some());
    }

    #[test]
    fn unchanged_selection_keeps_document_until_explicit_refresh() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("notes.txt");
        fs::write(&path, "original").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let original = app.preview().document().unwrap().clone();
        fs::write(&path, "changed").unwrap();

        app.move_down();
        app.cycle_sort();
        // Moving and sorting the same selection do not reload its source.
        assert_eq!(app.preview().document(), Some(&original));

        app.refresh();
        assert_ne!(app.preview().document(), Some(&original));
        assert_eq!(
            app.preview().document(),
            Some(&crate::document::load(&path, &app.theme().palette()).unwrap())
        );
    }

    #[test]
    fn leaving_an_image_cancels_its_pending_preview() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("a.png"), "invalid image").unwrap();
        fs::write(temp.path().join("b.txt"), "selected text").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        let first_request = app.image_request.unwrap();

        app.move_down();
        assert!(app.image_request.is_none());
        assert!(!app.image_pending());
        assert_eq!(
            app.preview().document().unwrap().lines[0].to_string(),
            "selected text"
        );
        app.move_up();
        assert_ne!(app.image_request, Some(first_request));
        assert!(app.preview().document().is_none());
    }

    fn finish_search(app: &mut App) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while app.search_pending() {
            app.poll_search();
            assert!(std::time::Instant::now() < deadline, "search timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
        app.poll_search();
    }

    #[test]
    fn search_recurses_and_cancel_restores_exact_folder_and_selection() {
        let (temp, mut app) = fixture();
        app.move_down();
        let selected = app.selected_entry().unwrap().clone();
        fs::write(temp.path().join("Alpha/inside.txt"), "zero\nNeedle here\n").unwrap();
        app.begin_search();
        for character in "NEEDLE".chars() {
            app.push_search_char(character);
        }
        finish_search(&mut app);
        assert!(app.is_searching());
        assert_eq!(app.search_query(), "NEEDLE");
        assert_eq!(app.entries().len(), 2);
        assert_eq!(app.selected_search_hit().unwrap().line, Some(2));
        assert_eq!(app.activate_selected(), NavigationResult::Continue);
        assert!(app.is_viewing());
        assert_eq!(app.viewer_source_line, Some(2));
        app.reveal_viewer_source(ratatui::layout::Rect::new(0, 0, 40, 10));
        assert_eq!(crate::ui::viewer_source_at_scroll(&app), Some(2));
        app.close_viewer();
        app.open_editor();
        assert_eq!(app.editor().unwrap().cursor(), (1, 0));
        assert_eq!(app.editor().unwrap().lines()[1], "Needle here");
        app.handle_editor_key(termina::event::KeyCode::Escape.into(), 5);
        assert!(app.is_searching());
        app.cancel_search();
        assert_eq!(app.current_dir(), temp.path());
        assert_eq!(app.selected_entry(), Some(&selected));
        assert!(!app.is_searching());
    }

    #[test]
    fn content_activation_uses_logical_source_line_after_lone_cr() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("notes.txt"), "a\rb\nneedle\r\n").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_search();
        for ch in "needle".chars() {
            app.push_search_char(ch);
        }
        finish_search(&mut app);
        assert_eq!(app.selected_search_hit().unwrap().line, Some(3));
        assert_eq!(
            app.selected_search_hit().unwrap().kind,
            crate::search::SearchHitKind::File
        );
        assert_eq!(app.activate_selected(), NavigationResult::Continue);
        app.reveal_viewer_source(ratatui::layout::Rect::new(0, 0, 40, 10));
        assert_eq!(crate::ui::viewer_source_at_scroll(&app), Some(3));
        assert_eq!(
            app.preview().document().unwrap().lines[2].to_string(),
            "needle"
        );
        app.close_viewer();
        app.cancel_search();
        assert_eq!(app.selected_entry().unwrap().name, "notes.txt");
    }

    #[test]
    fn search_suspends_idle_wakeups_in_viewer_and_editor_then_resumes() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("notes.txt"), "needle\n".repeat(300)).unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_search();
        for ch in "needle".chars() {
            app.push_search_char(ch);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while app.entries().is_empty() {
            app.poll_search();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(app.search_pending());
        assert!(app.background_work_pending());
        app.open_viewer();
        assert!(!app.background_work_pending());
        assert!(!app.poll_search());
        app.close_viewer();
        assert!(app.background_work_pending());
        app.open_editor();
        assert!(!app.background_work_pending());
        assert!(!app.poll_search());
        app.handle_editor_key(termina::event::KeyCode::Escape.into(), 5);
        assert!(app.background_work_pending());
        finish_search(&mut app);
        assert_eq!(app.entries().len(), 301);
        assert!(!app.background_work_pending());
    }

    #[test]
    fn no_match_activation_does_not_accept_any_folder() {
        let (_temp, mut app) = fixture();
        app.begin_search();
        for ch in "not-present".chars() {
            app.push_search_char(ch);
        }
        finish_search(&mut app);
        assert_eq!(app.activate_selected(), NavigationResult::Continue);
        assert!(!app.is_viewing());
        app.cancel_search();
        app.enter_selected();
        app.begin_search();
        app.go_parent();
        app.cancel_search();
        assert!(app.current_dir().ends_with("Alpha"));
        assert_eq!(app.selected_entry().unwrap().name, "inside.txt");
    }

    #[test]
    fn editing_from_picker_and_rendered_viewer_uses_original_source() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("notes.md");
        fs::write(&path, "# Unicode λ\r\nno final newline").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_editor();
        assert!(!app.is_viewing());
        assert_eq!(app.editor().unwrap().lines()[0], "# Unicode λ");
        app.handle_editor_key(termina::event::KeyCode::Escape.into(), 5);
        assert!(app.editor().is_none());
        app.open_viewer();
        assert!(app.is_rendered_markdown_view());
        app.open_editor();
        assert!(app.editor().is_some());
        app.paste("new ");
        app.handle_editor_key(termina::event::KeyCode::Escape.into(), 5);
        assert!(app.editor().unwrap().confirming());
        app.handle_editor_key(termina::event::KeyCode::Char('c').into(), 5);
        assert!(!app.editor().unwrap().confirming());
        app.handle_editor_key(termina::event::KeyCode::Escape.into(), 5);
        app.handle_editor_key(termina::event::KeyCode::Char('d').into(), 5);
        assert!(app.editor().is_none());
        assert!(app.is_rendered_markdown_view());
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "# Unicode λ\r\nno final newline"
        );
    }

    #[test]
    fn editor_conflict_keeps_dirty_buffer_and_cancel_returns_to_editing() {
        use termina::event::{KeyCode, KeyEvent, Modifiers};
        let temp = tempdir().unwrap();
        let path = temp.path().join("notes.txt");
        fs::write(&path, "original").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.open_editor();
        app.paste("local ");
        fs::write(&path, "external").unwrap();
        app.handle_editor_key(KeyEvent::new(KeyCode::Char('s'), Modifiers::CONTROL), 5);
        assert!(app.editor().unwrap().dirty());
        assert!(app.editor().unwrap().message().unwrap().contains("changed"));
        app.handle_editor_key(KeyCode::Escape.into(), 5);
        assert!(app.editor().unwrap().confirming());
        app.handle_editor_key(KeyCode::Char('s').into(), 5);
        assert!(app.editor().unwrap().confirming());
        app.handle_editor_key(KeyCode::Char('c').into(), 5);
        assert!(!app.editor().unwrap().confirming());
        assert_eq!(app.editor().unwrap().lines(), &["local original"]);
        assert_eq!(fs::read_to_string(path).unwrap(), "external");
        assert!(!app.is_viewing());
    }

    #[test]
    fn arriving_search_batches_preserve_selected_path_and_line() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("a")).unwrap();
        for index in 0..260 {
            fs::write(temp.path().join(format!("a/needle-{index:03}.txt")), "text").unwrap();
        }
        // DFS finds this later, but path ordering puts it before a/needle-*.
        fs::write(temp.path().join("a.needle"), "text").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_search();
        for ch in "needle".chars() {
            app.push_search_char(ch);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while app.entries().is_empty() {
            app.poll_search();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        app.move_last();
        let selected = app.selected_search_hit().unwrap().clone();
        finish_search(&mut app);
        assert_eq!(app.entries().len(), 261);
        assert_eq!(app.selected_search_hit(), Some(&selected));
    }

    #[test]
    fn search_scope_maps_selection_and_keeps_session_scope() {
        use crate::search::{SearchHitKind, SearchScope};
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("needle-folder")).unwrap();
        fs::write(temp.path().join("needle-folder/child.txt"), "plain").unwrap();
        fs::write(
            temp.path().join("needle.txt"),
            "first needle\nskip\nlast needle\n",
        )
        .unwrap();
        fs::write(temp.path().join("other.txt"), "needle\n").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_search();
        app.paste("needle");
        finish_search(&mut app);
        assert_eq!(app.search_counts(), (3, 3, 2));
        let name = app
            .search_hits
            .iter()
            .position(|hit| hit.path.ends_with("needle.txt") && hit.kind == SearchHitKind::Name)
            .unwrap();
        assert_eq!(app.search_file_line_count(name), 2);
        assert_eq!(app.search_file_line_count(name + 1), 0);

        // An unmatched directory has no content row: completion falls back to the first group.
        app.cycle_search_scope();
        finish_search(&mut app);
        assert_eq!(app.search_counts(), (3, 0, 0));
        app.cycle_search_scope();
        finish_search(&mut app);
        assert_eq!(app.search_scope(), SearchScope::Content);
        assert_eq!(app.search_counts(), (0, 3, 2));
        assert!(app.selected_entry().unwrap().path.ends_with("needle.txt"));
        assert_eq!(app.selected_search_hit().unwrap().kind, SearchHitKind::File);
        assert!(app.search_selection.is_none());

        app.page_down(2);
        assert_eq!(app.selected_search_hit().unwrap().line, Some(3));
        app.cycle_search_scope();
        finish_search(&mut app);
        assert_eq!(app.selected_search_hit().unwrap().kind, SearchHitKind::Line);
        assert_eq!(app.selected_search_hit().unwrap().line, Some(3));
        app.cycle_search_scope();
        finish_search(&mut app);
        assert_eq!(app.selected_search_hit().unwrap().kind, SearchHitKind::Name);
        app.cycle_search_scope();
        finish_search(&mut app);
        assert_eq!(app.selected_search_hit().unwrap().kind, SearchHitKind::Line);
        assert_eq!(app.selected_search_hit().unwrap().line, Some(1));
        assert_eq!(app.search_query(), "needle");
        app.cancel_search();
        assert_eq!(app.search_scope(), SearchScope::Content);
        app.enter_selected();
        assert!(app.current_dir().ends_with("needle-folder"));
        app.begin_search();
        assert_eq!(app.search_scope(), SearchScope::Content);
        app.paste("plain");
        finish_search(&mut app);
        assert_eq!(app.search_counts(), (0, 1, 1));
    }

    #[test]
    fn pending_search_line_waits_for_its_batch_unless_navigation_cancels_it() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("a.txt"), "needle\n").unwrap();
        fs::write(temp.path().join("z.txt"), "needle\nskip\nneedle\n").unwrap();
        let mut app = App::new(temp.path().to_path_buf()).unwrap();
        app.begin_search();
        app.paste("needle");
        finish_search(&mut app);
        app.move_last();
        let wanted = app.selected_search_hit().unwrap().clone();
        let hits = app.search_hits.clone();
        let split = hits.len() - 1;

        // Replay real worker rows with the selected line in a later batch.
        app.request_search();
        app.search_worker.cancel();
        app.merge_search_hits(hits[..split].to_vec());
        app.restore_search_selection(None, false);
        assert_eq!(
            app.selected_search_hit().unwrap().kind,
            crate::search::SearchHitKind::File
        );
        assert!(app.search_selection.is_some());
        let selected = app.selected_search_hit().map(SearchSelection::from);
        app.merge_search_hits(hits[split..].to_vec());
        app.restore_search_selection(selected.as_ref(), true);
        assert_eq!(app.selected_search_hit(), Some(&wanted));
        assert!(app.search_selection.is_none());

        app.request_search();
        app.search_worker.cancel();
        app.merge_search_hits(hits[..split].to_vec());
        app.restore_search_selection(None, false);
        app.move_first();
        assert!(app.search_selection.is_none());
        let manual = app.selected_search_hit().unwrap().clone();
        let selected = app.selected_search_hit().map(SearchSelection::from);
        app.merge_search_hits(hits[split..].to_vec());
        app.restore_search_selection(selected.as_ref(), true);
        assert_eq!(app.selected_search_hit(), Some(&manual));
        assert_eq!(app.search_counts(), (0, 3, 2));
    }

    #[test]
    fn page_navigation_clamps_to_the_available_entries() {
        let (_temp, mut app) = fixture();
        app.page_down(2);
        assert_eq!(app.selected(), 2);

        app.page_up(1);
        assert_eq!(app.selected(), 1);

        app.page_down(99);
        assert_eq!(app.selected(), 2);
        app.page_up(99);
        assert_eq!(app.selected(), 0);
    }

    #[test]
    fn sort_criterion_and_direction_are_independent_and_preserve_selection() {
        let (_temp, mut app) = fixture();
        for entry in &mut app.all_entries {
            let seconds = match entry.name.to_string_lossy().as_ref() {
                "Alpha" => 10,
                "zeta" => 20,
                "notes.txt" => 30,
                _ => 0,
            };
            entry.modified = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds));
        }
        app.entries = app.all_entries.clone();

        assert_eq!(app.sort_mode(), SortMode::Name);
        assert_eq!(app.sort_direction(), SortDirection::Ascending);
        assert_eq!(app.selected_entry().unwrap().display_name(), "Alpha/");

        app.cycle_sort();
        let names: Vec<_> = app.entries().iter().map(Entry::display_name).collect();
        assert_eq!(app.sort_mode(), SortMode::Modified);
        assert_eq!(app.sort_direction(), SortDirection::Ascending);
        assert_eq!(names, ["Alpha/", "zeta/", "notes.txt"]);
        assert_eq!(app.selected_entry().unwrap().display_name(), "Alpha/");

        app.toggle_sort_direction();
        let names: Vec<_> = app.entries().iter().map(Entry::display_name).collect();
        assert_eq!(app.sort_mode(), SortMode::Modified);
        assert_eq!(app.sort_direction(), SortDirection::Descending);
        assert_eq!(names, ["zeta/", "Alpha/", "notes.txt"]);
        assert_eq!(app.selected_entry().unwrap().display_name(), "Alpha/");
        assert_eq!(app.preview().label(), "Alpha/");

        app.cycle_sort();
        let names: Vec<_> = app.entries().iter().map(Entry::display_name).collect();
        assert_eq!(app.sort_mode(), SortMode::Name);
        assert_eq!(app.sort_direction(), SortDirection::Descending);
        assert_eq!(names, ["zeta/", "Alpha/", "notes.txt"]);
        assert_eq!(app.selected_entry().unwrap().display_name(), "Alpha/");

        let mut incomplete_metadata = vec![
            Entry {
                name: "unknown".into(),
                path: "unknown".into(),
                is_dir: false,
                is_symlink: false,
                modified: None,
            },
            Entry {
                name: "known".into(),
                path: "known".into(),
                is_dir: false,
                is_symlink: false,
                modified: Some(SystemTime::UNIX_EPOCH),
            },
        ];
        sort_entries(
            &mut incomplete_metadata,
            SortMode::Modified,
            SortDirection::Ascending,
        );
        assert_eq!(incomplete_metadata[0].name, "known");
        sort_entries(
            &mut incomplete_metadata,
            SortMode::Modified,
            SortDirection::Descending,
        );
        assert_eq!(incomplete_metadata[0].name, "known");
    }
}
