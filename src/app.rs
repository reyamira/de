use crate::{BrowseDefaults, DisplaySettings, Theme};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
pub struct App {
    current_dir: PathBuf,
    all_entries: Vec<Entry>,
    entries: Vec<Entry>,
    selected: usize,
    show_hidden: bool,
    status: Option<String>,
    preview: Preview,
    filtering: bool,
    filter: String,
    sort_mode: SortMode,
    sort_direction: SortDirection,
    theme: Theme,
    display: DisplaySettings,
    viewing: bool,
    viewer_scroll: usize,
    markdown_source: bool,
    image_loader: Option<crate::image_loader::ImageLoader>,
    image_request: Option<u64>,
    viewer_text_cache: RefCell<Option<crate::ui::ViewerText>>,
    markdown_renderer: Option<crate::markdown_renderer::MarkdownRenderer>,
    markdown_request: Option<u64>,
    markdown_key: Option<(u32, u32, u32, bool)>,
    markdown_page: Option<crate::markdown_renderer::RenderedPage>,
    markdown_error: Option<String>,
    markdown_cell_height: usize,
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
            filtering: false,
            filter: String::new(),
            sort_mode,
            sort_direction,
            theme: Theme::auto(),
            display: DisplaySettings::default(),
            viewing: false,
            viewer_scroll: 0,
            markdown_source: false,
            image_loader: None,
            image_request: None,
            viewer_text_cache: RefCell::new(None),
            markdown_renderer: None,
            markdown_request: None,
            markdown_key: None,
            markdown_page: None,
            markdown_error: None,
            markdown_cell_height: 16,
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

    pub fn is_markdown_source(&self) -> bool {
        self.markdown_source
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
                .is_some_and(|document| document.source_lines.is_some())
        {
            self.markdown_source = !self.markdown_source;
            self.viewer_scroll = 0;
            self.cancel_markdown();
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
        let dark = self.theme.name() != "light";
        let key = (width, height, scroll, dark);
        if self.markdown_key == Some(key) {
            return;
        }
        if self
            .markdown_key
            .is_some_and(|old| old.0 != width || old.1 != height || old.3 != dark)
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
        let renderer = self
            .markdown_renderer
            .get_or_insert_with(crate::markdown_renderer::MarkdownRenderer::new);
        self.markdown_request = Some(renderer.request(crate::markdown_renderer::RenderRequest {
            path,
            source,
            width,
            height,
            scroll_y: scroll,
            dark,
        }));
        self.markdown_key = Some(key);
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
        match result {
            Ok(page) => {
                self.viewer_scroll = page.scroll_y as usize;
                if let Some(key) = &mut self.markdown_key {
                    key.2 = page.scroll_y;
                }
                self.markdown_error = None;
                self.markdown_page = Some(page);
            }
            Err(error) => {
                self.markdown_page = None;
                self.markdown_error = Some(error);
            }
        }
        true
    }

    fn cancel_markdown(&mut self) {
        if let Some(renderer) = self.markdown_renderer.as_mut() {
            renderer.cancel();
        }
        self.markdown_request = None;
        self.markdown_key = None;
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
                self.viewer_scroll = 0;
                self.markdown_source = false;
                self.status = None;
            }
            _ => self.status = Some("select a file to view".into()),
        }
    }

    pub fn close_viewer(&mut self) {
        self.viewing = false;
        self.viewer_scroll = 0;
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

    pub fn is_filtering(&self) -> bool {
        self.filtering
    }

    pub fn filter_query(&self) -> &str {
        &self.filter
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
        if self.theme != theme && self.preview.document.is_some() {
            self.preview.path = None;
        }
        self.theme = theme;
        self.refresh_preview();
    }

    pub fn display_settings(&self) -> &DisplaySettings {
        &self.display
    }

    pub fn set_display_settings(&mut self, display: DisplaySettings) {
        self.display = display;
    }

    pub fn move_up(&mut self) {
        self.status = None;
        self.selected = self.selected.saturating_sub(1);
        self.refresh_preview();
    }

    pub fn move_down(&mut self) {
        self.status = None;
        if !self.entries.is_empty() {
            self.selected = (self.selected + 1).min(self.entries.len() - 1);
        }
        self.refresh_preview();
    }

    pub fn move_first(&mut self) {
        self.status = None;
        self.selected = 0;
        self.refresh_preview();
    }

    pub fn move_last(&mut self) {
        self.status = None;
        self.selected = self.entries.len().saturating_sub(1);
        self.refresh_preview();
    }

    pub fn page_up(&mut self, rows: usize) {
        self.status = None;
        self.selected = self.selected.saturating_sub(rows.max(1));
        self.refresh_preview();
    }

    pub fn page_down(&mut self, rows: usize) {
        self.status = None;
        if !self.entries.is_empty() {
            self.selected = self
                .selected
                .saturating_add(rows.max(1))
                .min(self.entries.len() - 1);
        }
        self.refresh_preview();
    }

    pub fn begin_filter(&mut self) {
        self.filtering = true;
        self.status = None;
    }

    pub fn push_filter_char(&mut self, character: char) {
        let selected_name = self.selected_entry().map(|entry| entry.name.clone());
        self.filter.push(character);
        self.apply_filter(selected_name.as_deref());
    }

    pub fn pop_filter_char(&mut self) {
        let selected_name = self.selected_entry().map(|entry| entry.name.clone());
        self.filter.pop();
        self.apply_filter(selected_name.as_deref());
    }

    pub fn clear_filter(&mut self) {
        let selected_name = self.selected_entry().map(|entry| entry.name.clone());
        self.filter.clear();
        self.filtering = false;
        self.apply_filter(selected_name.as_deref());
    }

    pub fn cycle_sort(&mut self) {
        let selected_name = self.selected_entry().map(|entry| entry.name.clone());
        self.sort_mode = self.sort_mode.next();
        sort_entries(&mut self.all_entries, self.sort_mode, self.sort_direction);
        self.apply_filter(selected_name.as_deref());
    }

    pub fn toggle_sort_direction(&mut self) {
        let selected_name = self.selected_entry().map(|entry| entry.name.clone());
        self.sort_direction = self.sort_direction.opposite();
        sort_entries(&mut self.all_entries, self.sort_mode, self.sort_direction);
        self.apply_filter(selected_name.as_deref());
    }

    pub fn enter_selected(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        if !entry.is_dir {
            self.status = Some("use o to open files; de only enters directories".into());
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

    pub fn accept(&self) -> NavigationResult {
        NavigationResult::Accept(self.current_dir.clone())
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
                self.filter.clear();
                self.filtering = false;
                self.all_entries = entries;
                self.entries = self.all_entries.clone();
                self.selected = select_name
                    .and_then(|name| self.entries.iter().position(|entry| entry.name == name))
                    .unwrap_or(0);
                self.status = None;
                self.refresh_preview();
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
                self.apply_filter(selected_name.as_deref());
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

    fn apply_filter(&mut self, selected_name: Option<&OsStr>) {
        let query = self.filter.to_lowercase();
        self.entries = if query.is_empty() {
            self.all_entries.clone()
        } else {
            self.all_entries
                .iter()
                .filter(|entry| entry.name.to_string_lossy().to_lowercase().contains(&query))
                .cloned()
                .collect()
        };
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
        if !is_dir && self.preview.path.as_deref() == Some(path.as_path()) {
            return;
        }
        self.cancel_image_request();
        self.cancel_markdown();
        self.viewer_text_cache.get_mut().take();
        self.markdown_source = false;
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
            match crate::document::load(&path, &self.theme.palette()) {
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
        assert!(app.status().unwrap().contains("only enters directories"));
    }

    #[test]
    fn accept_returns_the_displayed_directory_not_the_highlighted_child() {
        let (temp, app) = fixture();
        assert_eq!(
            app.accept(),
            NavigationResult::Accept(temp.path().to_path_buf())
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
        app.begin_filter();
        app.push_filter_char('n');
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

    #[test]
    fn filter_is_case_insensitive_scoped_and_reversible() {
        let (_temp, mut app) = fixture();
        app.begin_filter();
        for character in "ALP".chars() {
            app.push_filter_char(character);
        }

        assert!(app.is_filtering());
        assert_eq!(app.filter_query(), "ALP");
        assert_eq!(app.total_entry_count(), 3);
        assert_eq!(app.entries().len(), 1);
        assert_eq!(app.selected_entry().unwrap().display_name(), "Alpha/");
        assert_eq!(app.preview().label(), "Alpha/");

        app.pop_filter_char();
        assert_eq!(app.filter_query(), "AL");
        app.clear_filter();
        assert!(!app.is_filtering());
        assert!(app.filter_query().is_empty());
        assert_eq!(app.entries().len(), 3);
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
