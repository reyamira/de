use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub name: OsString,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
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

#[derive(Debug)]
pub struct App {
    current_dir: PathBuf,
    entries: Vec<Entry>,
    selected: usize,
    show_hidden: bool,
    status: Option<String>,
    preview: Preview,
}

impl App {
    pub fn new(start: PathBuf) -> io::Result<Self> {
        if !start.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("not a directory: {}", start.display()),
            ));
        }

        let entries = read_entries(&start, false)?;
        let mut app = Self {
            current_dir: start,
            entries,
            selected: 0,
            show_hidden: false,
            status: None,
            preview: Preview {
                label: "preview".into(),
                entries: Vec::new(),
                message: None,
            },
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
        match read_entries(&target, self.show_hidden) {
            Ok(entries) => {
                self.current_dir = target;
                self.entries = entries;
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
        match read_entries(&self.current_dir, self.show_hidden) {
            Ok(entries) => {
                self.entries = entries;
                self.selected = selected_name
                    .as_ref()
                    .and_then(|name| self.entries.iter().position(|entry| &entry.name == name))
                    .unwrap_or(0);
                self.status = None;
                self.refresh_preview();
            }
            Err(error) => {
                self.status = Some(format!(
                    "cannot refresh {}: {error}",
                    self.current_dir.display()
                ));
            }
        }
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

        match read_entries(&entry.path, self.show_hidden) {
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

fn read_entries(path: &Path, show_hidden: bool) -> io::Result<Vec<Entry>> {
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
        let is_symlink = file_type.is_symlink();
        let is_dir = file_type.is_dir()
            || (is_symlink && dir_entry.metadata().is_ok_and(|metadata| metadata.is_dir()));

        entries.push(Entry {
            name,
            path: dir_entry.path(),
            is_dir,
            is_symlink,
        });
    }

    entries.sort_by(compare_entries);
    Ok(entries)
}

fn compare_entries(left: &Entry, right: &Entry) -> Ordering {
    right.is_dir.cmp(&left.is_dir).then_with(|| {
        left.name
            .to_string_lossy()
            .to_lowercase()
            .cmp(&right.name.to_string_lossy().to_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    })
}

fn is_hidden(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
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
}
