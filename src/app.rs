use crate::Theme;
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
    entries: Vec<Entry>,
    message: Option<String>,
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
}

impl App {
    pub fn new(start: PathBuf) -> io::Result<Self> {
        if !start.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("not a directory: {}", start.display()),
            ));
        }

        let sort_mode = SortMode::Name;
        let sort_direction = SortDirection::Ascending;
        let entries = read_entries(&start, false, sort_mode, sort_direction)?;
        let mut app = Self {
            current_dir: start,
            all_entries: entries.clone(),
            entries,
            selected: 0,
            show_hidden: false,
            status: None,
            preview: Preview {
                label: "preview".into(),
                entries: Vec::new(),
                message: None,
            },
            filtering: false,
            filter: String::new(),
            sort_mode,
            sort_direction,
            theme: Theme::auto(),
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

    pub fn preview(&self) -> &Preview {
        &self.preview
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
        self.theme = theme;
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
            self.status = Some("files are shown for context; de only enters directories".into());
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
        self.reload_preserving_selection();
    }

    pub fn accept(&self) -> NavigationResult {
        NavigationResult::Accept(self.current_dir.clone())
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
            self.preview = Preview {
                label: "preview".into(),
                entries: Vec::new(),
                message: Some("nothing selected".into()),
            };
            return;
        };

        let label = entry.display_name();
        if !entry.is_dir {
            self.preview = Preview {
                label,
                entries: Vec::new(),
                message: Some("file · shown for context only".into()),
            };
            return;
        }

        match read_entries(
            &entry.path,
            self.show_hidden,
            self.sort_mode,
            self.sort_direction,
        ) {
            Ok(entries) => {
                self.preview = Preview {
                    label,
                    message: entries.is_empty().then(|| "empty directory".into()),
                    entries,
                };
            }
            Err(error) => {
                self.preview = Preview {
                    label,
                    entries: Vec::new(),
                    message: Some(format!("cannot preview: {error}")),
                };
            }
        }
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
        assert_eq!(
            app.preview().message(),
            Some("file · shown for context only")
        );
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
