//! A bounded, in-process UTF-8 editor. No command opens an external editor.
//!
//! Files must be regular, writable, single-link files, at most 1 MiB and 20,000
//! logical lines (including an empty final line). The final path component must
//! not be a symlink. Loading uses `O_NOFOLLOW` and a bounded read. UTF-8 BOMs,
//! uniform LF/CRLF, final-newline presence, and Unix permission bits survive
//! saving. Mixed newlines, lone CR, NUL, and Unicode control characters other
//! than tab/newline are rejected. Newline-free files use LF for new lines.
//!
//! Paste accepts LF, CRLF, or CR, normalizes them to the file's newline policy, and
//! rejects the whole edit if controls or size limits would make it unsafe.
//! Edits store only replacement deltas, not full-buffer history snapshots.
//! Positions and selections use zero-based source lines and UTF-8 byte offsets;
//! selection endpoints are sorted and the end is exclusive. Tabs use four-column
//! stops for vertical cursor movement. The clipboard is private to this editor.
//!
//! Saving writes and syncs a temporary file in the same directory, restores its
//! permissions, checks the source's identity, metadata, and exact original bytes,
//! then atomically replaces it. Failed saves leave the buffer and history intact.
//! POSIX replacement has no compare-and-swap: another writer can still race the
//! final check. This is conflict detection, not a lock against hostile writers.
//! Parent-directory symlinks resolve at open; directory replacement is not locked.
//! Atomic replacement does not preserve ownership, ACLs, extended attributes, or
//! inode identity. Directory-entry crash durability is not guaranteed.

use std::fs::{self, File, Metadata};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags};
use termina::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const MAX_EDITOR_BYTES: usize = 1024 * 1024;
pub const MAX_EDITOR_LINES: usize = 20_000;
pub const EDITOR_TAB_WIDTH: usize = 4;
type Position = (usize, usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorOutcome {
    Continue,
    Close,
}

#[derive(Clone, Copy)]
struct Caret {
    cursor: Position,
    anchor: Option<Position>,
}

struct Delta {
    start: Position,
    removed: String,
    inserted: String,
    before: Caret,
    after: Caret,
    before_state: u64,
    after_state: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    length: u64,
    mode: u32,
    links: u64,
    uid: u32,
    gid: u32,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

pub struct Editor {
    path: PathBuf,
    lines: Vec<String>,
    caret: Caret,
    preferred_column: Option<usize>,
    normalized_bytes: usize,
    crlf: bool,
    bom: bool,
    original: Vec<u8>,
    identity: Identity,
    clipboard: String,
    undo: Vec<Delta>,
    redo: Vec<Delta>,
    revision: u64,
    changed_line: usize,
    state: u64,
    saved_state: u64,
    message: Option<String>,
    confirming: bool,
}

impl std::fmt::Debug for Editor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Editor")
            .field("path", &self.path)
            .field("cursor", &self.caret.cursor)
            .field("dirty", &self.dirty())
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

impl Editor {
    /// Open an existing file. `line` is one-based and clamps to the file bounds.
    pub fn open(path: PathBuf, line: usize) -> Result<Self, String> {
        let name = path.file_name().ok_or("The editor needs a file path")?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let path = parent
            .canonicalize()
            .map_err(|error| format!("Cannot resolve file directory: {error}"))?
            .join(name);
        let (original, metadata) = read_source(&path)?;
        let bom = original.starts_with(b"\xef\xbb\xbf");
        let text = std::str::from_utf8(&original[if bom { 3 } else { 0 }..])
            .map_err(|_| "The editor only supports valid UTF-8 files".to_owned())?;
        let (normalized, crlf) = normalize_source(text)?;
        let normalized_bytes = normalized.len();
        let lines: Vec<String> = normalized.split('\n').map(str::to_owned).collect();
        if lines.len() > MAX_EDITOR_LINES {
            return Err("The editor supports at most 20,000 lines".into());
        }
        let cursor = (line.saturating_sub(1).min(lines.len() - 1), 0);
        Ok(Self {
            path,
            lines,
            caret: Caret {
                cursor,
                anchor: None,
            },
            preferred_column: None,
            normalized_bytes,
            crlf,
            bom,
            original,
            identity: Identity::of(&metadata),
            clipboard: String::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            revision: 0,
            changed_line: 0,
            state: 0,
            saved_state: 0,
            message: None,
            confirming: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn dirty(&self) -> bool {
        self.state != self.saved_state
    }
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
    pub fn confirming(&self) -> bool {
        self.confirming
    }
    pub fn cursor(&self) -> Position {
        self.caret.cursor
    }
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Earliest source line touched by the most recent content revision.
    pub fn changed_line(&self) -> usize {
        self.changed_line
    }
    pub fn uses_crlf(&self) -> bool {
        self.crlf
    }
    pub fn has_bom(&self) -> bool {
        self.bom
    }

    pub fn selection(&self) -> Option<(Position, Position)> {
        self.caret
            .anchor
            .filter(|anchor| *anchor != self.caret.cursor)
            .map(|anchor| (anchor.min(self.caret.cursor), anchor.max(self.caret.cursor)))
    }

    /// The caret's display column, using grapheme widths and four-column tabs.
    pub fn cursor_column(&self) -> usize {
        display_column(&self.lines[self.caret.cursor.0][..self.caret.cursor.1])
    }

    pub fn handle_key(&mut self, key: KeyEvent, page_rows: usize) -> EditorOutcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return EditorOutcome::Continue;
        }
        let control = key.modifiers.contains(Modifiers::CONTROL);
        let shift = key.modifiers.contains(Modifiers::SHIFT);
        let alt = key.modifiers.contains(Modifiers::ALT);
        if self.confirming {
            match key.code {
                KeyCode::Escape => self.cancel_close(),
                KeyCode::Char(c) if !control && !alt => match c.to_ascii_lowercase() {
                    's' => {
                        if self.save().is_ok() {
                            return EditorOutcome::Close;
                        }
                    }
                    'd' => return EditorOutcome::Close,
                    'c' => self.cancel_close(),
                    _ => {}
                },
                _ => {}
            }
            return EditorOutcome::Continue;
        }
        if key.code == KeyCode::Escape || (control && matches!(key.code, KeyCode::Char('q' | 'Q')))
        {
            if !self.dirty() {
                return EditorOutcome::Close;
            }
            self.confirming = true;
            self.message = Some("Save changes? [s] Save  [d] Discard  [c/Escape] Cancel".into());
            return EditorOutcome::Continue;
        }
        if alt {
            return EditorOutcome::Continue;
        }
        if control && let KeyCode::Char(character) = key.code {
            match character.to_ascii_lowercase() {
                's' => {
                    let _ = self.save();
                }
                'c' => self.copy(),
                'x' => {
                    self.copy();
                    if self.selection().is_some() {
                        self.insert("");
                    }
                }
                'v' if !self.clipboard.is_empty() => {
                    // Moving the clipboard avoids copying it merely to borrow self.
                    let clipboard = std::mem::take(&mut self.clipboard);
                    self.insert(&clipboard);
                    self.clipboard = clipboard;
                }
                'a' => {
                    self.caret.anchor = Some((0, 0));
                    self.caret.cursor = self.end();
                    self.preferred_column = None;
                }
                'z' if shift => self.redo(),
                'z' => self.undo(),
                'y' => self.redo(),
                _ => {}
            }
            return EditorOutcome::Continue;
        }
        match key.code {
            KeyCode::Left => {
                let next = if !shift && self.selection().is_some() {
                    self.selection().unwrap().0
                } else if control {
                    self.word_left()
                } else {
                    self.previous(self.caret.cursor)
                };
                self.move_to(next, shift, false);
            }
            KeyCode::Right => {
                let next = if !shift && self.selection().is_some() {
                    self.selection().unwrap().1
                } else if control {
                    self.word_right()
                } else {
                    self.next(self.caret.cursor)
                };
                self.move_to(next, shift, false);
            }
            KeyCode::Up => self.vertical(self.caret.cursor.0.saturating_sub(1), shift),
            KeyCode::Down => {
                self.vertical((self.caret.cursor.0 + 1).min(self.lines.len() - 1), shift)
            }
            KeyCode::PageUp => {
                self.vertical(self.caret.cursor.0.saturating_sub(page_rows.max(1)), shift)
            }
            KeyCode::PageDown => self.vertical(
                self.caret
                    .cursor
                    .0
                    .saturating_add(page_rows.max(1))
                    .min(self.lines.len() - 1),
                shift,
            ),
            KeyCode::Home => self.move_to(
                if control {
                    (0, 0)
                } else {
                    (self.caret.cursor.0, 0)
                },
                shift,
                false,
            ),
            KeyCode::End => self.move_to(
                if control {
                    self.end()
                } else {
                    (self.caret.cursor.0, self.lines[self.caret.cursor.0].len())
                },
                shift,
                false,
            ),
            KeyCode::Backspace => self.delete(true, control),
            KeyCode::Delete => self.delete(false, control),
            KeyCode::Enter if !control => self.insert("\n"),
            KeyCode::Tab if !control => self.insert("\t"),
            KeyCode::Char(character) if !control && !character.is_control() => {
                let mut bytes = [0; 4];
                self.insert(character.encode_utf8(&mut bytes));
            }
            _ => {}
        }
        EditorOutcome::Continue
    }

    /// Paste is one undoable edit. A confirmation prompt does not accept paste.
    pub fn paste(&mut self, text: &str) {
        if self.confirming {
            return;
        }
        if text.len() > MAX_EDITOR_BYTES * 2 {
            self.message = Some("Paste exceeds the 1 MiB editor limit".into());
            return;
        }
        match normalize_paste(text) {
            Ok(normalized) => self.insert(&normalized),
            Err(error) => self.message = Some(error),
        }
    }

    pub fn save(&mut self) -> Result<(), String> {
        let result = self.save_inner();
        self.message = Some(match &result {
            Ok(()) => "Saved".into(),
            Err(error) => error.clone(),
        });
        result
    }

    fn save_inner(&mut self) -> Result<(), String> {
        let mut encoded =
            Vec::with_capacity(self.encoded_len(self.normalized_bytes, self.lines.len()));
        if self.bom {
            encoded.extend_from_slice(b"\xef\xbb\xbf");
        }
        for (index, line) in self.lines.iter().enumerate() {
            if index != 0 {
                encoded.extend_from_slice(if self.crlf { b"\r\n" } else { b"\n" });
            }
            encoded.extend_from_slice(line.as_bytes());
        }
        let parent = self
            .path
            .parent()
            .ok_or("The file has no parent directory")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| format!("Cannot create a temporary file: {error}"))?;
        temporary
            .write_all(&encoded)
            .map_err(|error| format!("Cannot write file: {error}"))?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(self.identity.mode & 0o7777))
            .map_err(|error| format!("Cannot preserve file permissions: {error}"))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| format!("Cannot sync file: {error}"))?;
        let replacement_identity = Identity::of(
            &temporary
                .as_file()
                .metadata()
                .map_err(|error| format!("Cannot inspect temporary file: {error}"))?,
        );
        let (current, metadata) = read_source(&self.path)
            .map_err(|error| format!("Cannot save; source check failed: {error}"))?;
        if Identity::of(&metadata) != self.identity || current != self.original {
            return Err("Cannot save: the file changed outside the editor".into());
        }
        // Recheck the pathname after reading, immediately before atomic replace.
        let named = fs::symlink_metadata(&self.path)
            .map_err(|error| format!("Cannot check source path: {error}"))?;
        if Identity::of(&named) != self.identity || !named.file_type().is_file() {
            return Err("Cannot save: the source path changed outside the editor".into());
        }
        let persisted = temporary
            .persist(&self.path)
            .map_err(|error| format!("Cannot replace file: {}", error.error))?;
        // Rename can update ctime. Prefer the persisted handle's fresh identity.
        self.identity = persisted
            .metadata()
            .map(|metadata| Identity::of(&metadata))
            .unwrap_or(replacement_identity);
        self.original = encoded;
        self.saved_state = self.state;
        Ok(())
    }

    fn cancel_close(&mut self) {
        self.confirming = false;
        self.message = None;
    }

    fn encoded_len(&self, bytes: usize, lines: usize) -> usize {
        bytes + if self.crlf { lines - 1 } else { 0 } + if self.bom { 3 } else { 0 }
    }

    fn end(&self) -> Position {
        (self.lines.len() - 1, self.lines.last().unwrap().len())
    }

    fn range_text(&self, start: Position, end: Position) -> String {
        if start.0 == end.0 {
            return self.lines[start.0][start.1..end.1].to_owned();
        }
        let mut text = self.lines[start.0][start.1..].to_owned();
        for line in &self.lines[start.0 + 1..end.0] {
            text.push('\n');
            text.push_str(line);
        }
        text.push('\n');
        text.push_str(&self.lines[end.0][..end.1]);
        text
    }

    fn copy(&mut self) {
        if let Some((start, end)) = self.selection() {
            self.clipboard = self.range_text(start, end);
            self.message = Some("Selection copied to the editor clipboard".into());
        }
    }

    fn insert(&mut self, text: &str) {
        let (start, end) = self
            .selection()
            .unwrap_or((self.caret.cursor, self.caret.cursor));
        self.replace(start, end, text);
    }

    fn delete(&mut self, backward: bool, word: bool) {
        let (start, end) = self.selection().unwrap_or_else(|| {
            let cursor = self.caret.cursor;
            if backward {
                (
                    if word {
                        self.word_left()
                    } else {
                        self.previous(cursor)
                    },
                    cursor,
                )
            } else {
                (
                    cursor,
                    if word {
                        self.word_right()
                    } else {
                        self.next(cursor)
                    },
                )
            }
        });
        self.replace(start, end, "");
    }

    fn replace(&mut self, start: Position, end: Position, text: &str) {
        let removed_bytes = if start.0 == end.0 {
            end.1 - start.1
        } else {
            self.lines[start.0].len() - start.1
                + end.1
                + (end.0 - start.0)
                + self.lines[start.0 + 1..end.0]
                    .iter()
                    .map(String::len)
                    .sum::<usize>()
        };
        let inserted_lines = text.bytes().filter(|byte| *byte == b'\n').count();
        let line_count = self.lines.len() - (end.0 - start.0) + inserted_lines;
        let byte_count = self.normalized_bytes - removed_bytes + text.len();
        if line_count > MAX_EDITOR_LINES
            || self.encoded_len(byte_count, line_count) > MAX_EDITOR_BYTES
        {
            self.message = Some("Edit exceeds the 1 MiB or 20,000-line editor limit".into());
            return;
        }
        if start == end && text.is_empty() {
            return;
        }
        let removed = self.range_text(start, end);
        if removed == text {
            self.caret.cursor = position_after(start, text);
            self.caret.cursor = self.snap_forward(self.caret.cursor);
            self.caret.anchor = None;
            self.preferred_column = None;
            return;
        }
        let before = self.caret;
        self.replace_raw(start, end, text);
        self.caret = Caret {
            cursor: self.snap_forward(position_after(start, text)),
            anchor: None,
        };
        self.preferred_column = None;
        self.revision += 1;
        self.changed_line = start.0;
        self.undo.push(Delta {
            start,
            removed,
            inserted: text.to_owned(),
            before,
            after: self.caret,
            before_state: self.state,
            after_state: self.revision,
        });
        self.state = self.revision;
        self.redo.clear();
        self.message = None;
    }

    /// Replace only affected lines. The caller has checked limits and boundaries.
    fn replace_raw(&mut self, start: Position, end: Position, text: &str) {
        let removed_bytes = if start.0 == end.0 {
            end.1 - start.1
        } else {
            self.lines[start.0].len() - start.1 + end.1 + end.0 - start.0
                + self.lines[start.0 + 1..end.0]
                    .iter()
                    .map(String::len)
                    .sum::<usize>()
        };
        self.normalized_bytes = self.normalized_bytes - removed_bytes + text.len();
        if start.0 == end.0 && !text.contains('\n') {
            self.lines[start.0].replace_range(start.1..end.1, text);
            return;
        }
        let mut parts = text.split('\n');
        let first = parts.next().unwrap();
        let mut current = String::with_capacity(start.1 + first.len());
        current.push_str(&self.lines[start.0][..start.1]);
        current.push_str(first);
        let mut replacement = Vec::new();
        for part in parts {
            replacement.push(current);
            current = part.to_owned();
        }
        current.push_str(&self.lines[end.0][end.1..]);
        replacement.push(current);
        self.lines.splice(start.0..=end.0, replacement);
    }

    fn undo(&mut self) {
        if let Some(delta) = self.undo.pop() {
            self.replace_raw(
                delta.start,
                position_after(delta.start, &delta.inserted),
                &delta.removed,
            );
            self.caret = delta.before;
            self.state = delta.before_state;
            self.revision += 1;
            self.changed_line = delta.start.0;
            self.preferred_column = None;
            self.message = None;
            self.redo.push(delta);
        }
    }

    fn redo(&mut self) {
        if let Some(delta) = self.redo.pop() {
            self.replace_raw(
                delta.start,
                position_after(delta.start, &delta.removed),
                &delta.inserted,
            );
            self.caret = delta.after;
            self.state = delta.after_state;
            self.revision += 1;
            self.changed_line = delta.start.0;
            self.preferred_column = None;
            self.message = None;
            self.undo.push(delta);
        }
    }

    fn snap_forward(&self, position: Position) -> Position {
        let line = &self.lines[position.0];
        let mut cursor = unicode_segmentation::GraphemeCursor::new(position.1, line.len(), true);
        let byte = if cursor
            .is_boundary(line, 0)
            .expect("complete grapheme context")
        {
            position.1
        } else {
            cursor
                .next_boundary(line, 0)
                .expect("complete grapheme context")
                .unwrap_or(line.len())
        };
        (position.0, byte)
    }

    fn previous(&self, position: Position) -> Position {
        if position.1 == 0 {
            if position.0 == 0 {
                return position;
            }
            return (position.0 - 1, self.lines[position.0 - 1].len());
        }
        let line = &self.lines[position.0];
        let mut cursor = unicode_segmentation::GraphemeCursor::new(position.1, line.len(), true);
        let byte = cursor
            .prev_boundary(line, 0)
            .expect("complete grapheme context")
            .unwrap_or(0);
        (position.0, byte)
    }

    fn next(&self, position: Position) -> Position {
        let line = &self.lines[position.0];
        if position.1 == line.len() {
            return if position.0 + 1 < self.lines.len() {
                (position.0 + 1, 0)
            } else {
                position
            };
        }
        let mut cursor = unicode_segmentation::GraphemeCursor::new(position.1, line.len(), true);
        let byte = cursor
            .next_boundary(line, 0)
            .expect("complete grapheme context")
            .unwrap_or(line.len());
        (position.0, byte)
    }

    fn word_class(&self, position: Position) -> u8 {
        let line = &self.lines[position.0];
        if position.1 == line.len() {
            return 0;
        }
        let grapheme = line[position.1..].graphemes(true).next().unwrap();
        if grapheme.chars().all(char::is_whitespace) {
            0
        } else if grapheme.chars().any(|c| c.is_alphanumeric() || c == '_') {
            1
        } else {
            2
        }
    }

    fn word_left(&self) -> Position {
        let mut cursor = self.caret.cursor;
        while cursor != (0, 0) {
            let previous = self.previous(cursor);
            if self.word_class(previous) != 0 {
                break;
            }
            cursor = previous;
        }
        if cursor != (0, 0) {
            cursor = self.previous(cursor);
            let class = self.word_class(cursor);
            while cursor != (0, 0) {
                let previous = self.previous(cursor);
                if self.word_class(previous) != class {
                    break;
                }
                cursor = previous;
            }
        }
        cursor
    }

    fn word_right(&self) -> Position {
        let mut cursor = self.caret.cursor;
        let class = self.word_class(cursor);
        while cursor != self.end() && self.word_class(cursor) == class {
            cursor = self.next(cursor);
        }
        while cursor != self.end() && self.word_class(cursor) == 0 {
            cursor = self.next(cursor);
        }
        cursor
    }

    fn move_to(&mut self, position: Position, selecting: bool, vertical: bool) {
        if selecting {
            if self.caret.anchor.is_none() {
                self.caret.anchor = Some(self.caret.cursor);
            }
        } else {
            self.caret.anchor = None;
        }
        self.caret.cursor = position;
        if !vertical {
            self.preferred_column = None;
        }
    }

    fn vertical(&mut self, line: usize, selecting: bool) {
        let column = self
            .preferred_column
            .unwrap_or_else(|| self.cursor_column());
        self.preferred_column = Some(column);
        let mut width = 0;
        let mut byte = self.lines[line].len();
        for (offset, grapheme) in self.lines[line].grapheme_indices(true) {
            let next = width + grapheme_width(grapheme, width);
            if next > column {
                byte = offset;
                break;
            }
            width = next;
        }
        self.move_to((line, byte), selecting, true);
    }
}

fn position_after(start: Position, text: &str) -> Position {
    let newlines = text.bytes().filter(|byte| *byte == b'\n').count();
    match text.rfind('\n') {
        Some(last) => (start.0 + newlines, text.len() - last - 1),
        None => (start.0, start.1 + text.len()),
    }
}

fn grapheme_width(grapheme: &str, column: usize) -> usize {
    if grapheme == "\t" {
        EDITOR_TAB_WIDTH - column % EDITOR_TAB_WIDTH
    } else {
        UnicodeWidthStr::width(grapheme)
    }
}

fn display_column(text: &str) -> usize {
    text.graphemes(true).fold(0, |column, grapheme| {
        column + grapheme_width(grapheme, column)
    })
}

fn validate_metadata(metadata: &Metadata) -> Result<(), String> {
    if !metadata.is_file() {
        return Err("The editor only supports regular files, not symlinks".into());
    }
    if metadata.nlink() != 1 {
        return Err("The editor does not replace hardlinked files".into());
    }
    if metadata.permissions().readonly() || metadata.mode() & 0o222 == 0 {
        return Err("The file is read-only".into());
    }
    if metadata.len() > MAX_EDITOR_BYTES as u64 {
        return Err("The editor supports files up to 1 MiB".into());
    }
    Ok(())
}

fn read_source(path: &Path) -> Result<(Vec<u8>, Metadata), String> {
    let named =
        fs::symlink_metadata(path).map_err(|error| format!("Cannot inspect file: {error}"))?;
    validate_metadata(&named)?;
    let descriptor = rustix::fs::open(
        path,
        OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| format!("Cannot open file without following symlinks: {error}"))?;
    let mut file = File::from(descriptor);
    let before = file
        .metadata()
        .map_err(|error| format!("Cannot inspect open file: {error}"))?;
    validate_metadata(&before)?;
    if Identity::of(&before) != Identity::of(&named) {
        return Err("The file changed while opening it".into());
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_EDITOR_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot read file: {error}"))?;
    if bytes.len() > MAX_EDITOR_BYTES {
        return Err("The editor supports files up to 1 MiB".into());
    }
    let after = file
        .metadata()
        .map_err(|error| format!("Cannot inspect read file: {error}"))?;
    if Identity::of(&before) != Identity::of(&after) || bytes.len() as u64 != after.len() {
        return Err("The file changed while reading it".into());
    }
    Ok((bytes, after))
}

fn normalize_source(text: &str) -> Result<(String, bool), String> {
    let crlf = text.contains("\r\n");
    if text
        .as_bytes()
        .iter()
        .enumerate()
        .any(|(index, byte)| *byte == b'\r' && text.as_bytes().get(index + 1) != Some(&b'\n'))
    {
        return Err("Lone CR characters are not supported in source files".into());
    }
    let normalized = normalize_paste(text)?;
    if crlf
        && text
            .as_bytes()
            .windows(2)
            .filter(|pair| pair[1] == b'\n')
            .any(|pair| pair[0] != b'\r')
    {
        return Err("The editor does not support mixed LF and CRLF newlines".into());
    }
    if crlf && text.starts_with('\n') {
        return Err("The editor does not support mixed LF and CRLF newlines".into());
    }
    Ok((normalized, crlf))
}

fn normalize_paste(text: &str) -> Result<String, String> {
    let mut normalized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            let _ = characters.next_if_eq(&'\n');
            normalized.push('\n');
        } else if character.is_control() && character != '\n' && character != '\t' {
            return Err("Control characters other than tab and newline are not supported".into());
        } else {
            normalized.push(character);
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn fixture(bytes: &[u8]) -> (tempfile::TempDir, PathBuf, Editor) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("text.txt");
        fs::write(&path, bytes).unwrap();
        let editor = Editor::open(path.clone(), 1).unwrap();
        (directory, path, editor)
    }

    fn key(editor: &mut Editor, code: KeyCode, modifiers: Modifiers) -> EditorOutcome {
        editor.handle_key(KeyEvent::new(code, modifiers), 3)
    }

    fn plain(editor: &mut Editor, code: KeyCode) -> EditorOutcome {
        key(editor, code, Modifiers::NONE)
    }
    fn ctrl(editor: &mut Editor, character: char) -> EditorOutcome {
        key(editor, KeyCode::Char(character), Modifiers::CONTROL)
    }

    #[test]
    fn original_unicode_bom_crlf_and_no_final_newline_survive_save() {
        let bytes = "\u{feff}café 👩‍💻\r\n日本語".as_bytes();
        let (_directory, path, _editor) = fixture(bytes);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        // Reopen because permission changes are external source changes too.
        let mut editor = Editor::open(path.clone(), 2).unwrap();
        assert_eq!(editor.cursor(), (1, 0));
        assert!(editor.has_bom());
        assert!(editor.uses_crlf());
        editor.save().unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        plain(&mut editor, KeyCode::End);
        editor.paste("!\r\nfin");
        editor.save().unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            "\u{feff}café 👩‍💻\r\n日本語!\r\nfin".as_bytes()
        );
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o640);
        assert!(!editor.dirty());
        // A second save must accept the replacement inode and its fresh ctime.
        editor.save().unwrap();
    }

    #[test]
    fn final_newline_and_empty_files_survive() {
        for bytes in [b"".as_slice(), b"one\n", b"one\r\n", b"\xef\xbb\xbf"] {
            let (_directory, path, mut editor) = fixture(bytes);
            editor.save().unwrap();
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }

    #[test]
    fn conflicts_preserve_buffer_dirty_state_and_history() {
        let (_directory, path, mut editor) = fixture(b"original");
        editor.paste("new ");
        fs::write(&path, b"external").unwrap();
        assert!(editor.save().is_err());
        assert!(editor.dirty());
        assert_eq!(editor.lines(), &["new original"]);
        assert_eq!(fs::read(&path).unwrap(), b"external");
        ctrl(&mut editor, 'z');
        assert_eq!(editor.lines(), &["original"]);
        assert!(!editor.dirty());
    }

    #[test]
    fn replacement_with_identical_bytes_still_conflicts() {
        let (directory, path, mut editor) = fixture(b"same");
        editor.paste("edit");
        let other = directory.path().join("other");
        fs::write(&other, b"same").unwrap();
        fs::rename(other, &path).unwrap();
        assert!(editor.save().is_err());
        assert_eq!(fs::read(path).unwrap(), b"same");
        assert!(editor.dirty());
    }

    #[test]
    fn dirty_exit_has_save_discard_cancel_and_failed_save_states() {
        let (_directory, path, mut editor) = fixture(b"text");
        assert_eq!(plain(&mut editor, KeyCode::Escape), EditorOutcome::Close);
        editor.paste("x");
        assert_eq!(ctrl(&mut editor, 'q'), EditorOutcome::Continue);
        assert!(editor.confirming());
        editor.paste("ignored");
        assert_eq!(editor.lines(), &["xtext"]);
        assert_eq!(plain(&mut editor, KeyCode::Escape), EditorOutcome::Continue);
        assert!(!editor.confirming());
        plain(&mut editor, KeyCode::Escape);
        plain(&mut editor, KeyCode::Char('c'));
        assert!(!editor.confirming());
        plain(&mut editor, KeyCode::Escape);
        assert_eq!(plain(&mut editor, KeyCode::Char('s')), EditorOutcome::Close);
        assert_eq!(fs::read(&path).unwrap(), b"xtext");
        // A successful close drops the editor in App; reopen for the next session.
        let mut editor = Editor::open(path.clone(), 1).unwrap();
        editor.paste("y");
        fs::write(&path, b"external").unwrap();
        plain(&mut editor, KeyCode::Escape);
        assert_eq!(
            plain(&mut editor, KeyCode::Char('s')),
            EditorOutcome::Continue
        );
        assert!(editor.confirming());
        assert!(editor.dirty());
        assert!(editor.message().unwrap().contains("changed"));
        assert_eq!(plain(&mut editor, KeyCode::Char('d')), EditorOutcome::Close);
        assert_eq!(fs::read(path).unwrap(), b"external");
    }

    #[test]
    fn undo_redo_restores_selection_and_saved_state() {
        let (_directory, _path, mut editor) = fixture("aé\n👩‍💻z".as_bytes());
        ctrl(&mut editor, 'a');
        let selection = editor.selection();
        editor.paste("replacement\nline");
        assert!(editor.dirty());
        ctrl(&mut editor, 'z');
        assert_eq!(editor.lines(), &["aé", "👩‍💻z"]);
        assert_eq!(editor.selection(), selection);
        assert!(!editor.dirty());
        ctrl(&mut editor, 'y');
        assert_eq!(editor.lines(), &["replacement", "line"]);
        editor.save().unwrap();
        ctrl(&mut editor, 'z');
        assert!(editor.dirty());
        key(
            &mut editor,
            KeyCode::Char('Z'),
            Modifiers::CONTROL | Modifiers::SHIFT,
        );
        assert!(!editor.dirty());
        assert_eq!(editor.cursor(), (1, 4));
        assert_eq!(editor.revision(), 5);
    }

    #[test]
    fn internal_clipboard_selection_cut_and_paste_do_not_close() {
        let (_directory, _path, mut editor) = fixture(b"abc\ndef");
        key(&mut editor, KeyCode::Right, Modifiers::SHIFT);
        key(&mut editor, KeyCode::Right, Modifiers::SHIFT);
        assert_eq!(editor.selection(), Some(((0, 0), (0, 2))));
        assert_eq!(ctrl(&mut editor, 'c'), EditorOutcome::Continue);
        assert!(!editor.dirty());
        ctrl(&mut editor, 'x');
        assert_eq!(editor.lines(), &["c", "def"]);
        key(&mut editor, KeyCode::End, Modifiers::CONTROL);
        ctrl(&mut editor, 'v');
        assert_eq!(editor.lines(), &["c", "defab"]);
        assert_eq!(editor.selection(), None);
        ctrl(&mut editor, 'z');
        ctrl(&mut editor, 'z');
        assert_eq!(editor.lines(), &["abc", "def"]);
        assert!(!editor.dirty());
    }

    #[test]
    fn grapheme_navigation_deletes_whole_clusters() {
        let (_directory, _path, mut editor) = fixture("e\u{301}👩‍💻🇫🇷\nend".as_bytes());
        plain(&mut editor, KeyCode::Right);
        assert_eq!(editor.cursor(), (0, "e\u{301}".len()));
        plain(&mut editor, KeyCode::Delete);
        assert_eq!(editor.lines()[0], "e\u{301}🇫🇷");
        plain(&mut editor, KeyCode::Backspace);
        assert_eq!(editor.lines()[0], "🇫🇷");
        plain(&mut editor, KeyCode::Right);
        plain(&mut editor, KeyCode::Delete);
        assert_eq!(editor.lines(), &["🇫🇷end"]);
        ctrl(&mut editor, 'z');
        assert_eq!(editor.lines(), &["🇫🇷", "end"]);
    }

    #[test]
    fn arrows_home_end_pages_words_and_typing_are_editor_commands() {
        let (_directory, _path, mut editor) = fixture(b"one two\n\twide\nx\nlast");
        key(&mut editor, KeyCode::Right, Modifiers::CONTROL);
        assert_eq!(editor.cursor(), (0, 4));
        key(&mut editor, KeyCode::Left, Modifiers::CONTROL);
        assert_eq!(editor.cursor(), (0, 0));
        plain(&mut editor, KeyCode::End);
        plain(&mut editor, KeyCode::Down);
        assert_eq!(editor.cursor_column(), 7);
        plain(&mut editor, KeyCode::Down);
        assert_eq!(editor.cursor(), (2, 1));
        plain(&mut editor, KeyCode::Up);
        assert_eq!(editor.cursor_column(), 7);
        key(&mut editor, KeyCode::Home, Modifiers::CONTROL);
        plain(&mut editor, KeyCode::PageDown);
        assert_eq!(editor.cursor(), (3, 0));
        key(&mut editor, KeyCode::Home, Modifiers::CONTROL);
        for character in ['q', 'e', 'j', 'k'] {
            plain(&mut editor, KeyCode::Char(character));
        }
        plain(&mut editor, KeyCode::Tab);
        plain(&mut editor, KeyCode::Enter);
        assert_eq!(editor.lines()[0], "qejk\t");
        assert_eq!(editor.lines()[1], "one two");
    }

    #[test]
    fn invalid_sources_and_special_files_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source");
        for bytes in [
            b"a\r\nb\n".as_slice(),
            b"\na\r\n",
            b"a\rb",
            b"a\0b",
            b"a\x1bb",
            b"\xff",
        ] {
            fs::write(&path, bytes).unwrap();
            assert!(Editor::open(path.clone(), 1).is_err());
        }
        fs::write(&path, b"text").unwrap();
        let linked = directory.path().join("symlink");
        symlink(&path, &linked).unwrap();
        assert!(Editor::open(linked, 1).is_err());
        let hardlink = directory.path().join("hardlink");
        fs::hard_link(&path, &hardlink).unwrap();
        assert!(Editor::open(path.clone(), 1).is_err());
        fs::remove_file(hardlink).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        assert!(Editor::open(path, 1).is_err());
        assert!(Editor::open(directory.path().to_path_buf(), 1).is_err());
    }

    #[test]
    fn pasted_cr_crlf_and_lf_use_the_existing_file_newline_style() {
        for (source, expected) in [
            (b"tail".as_slice(), b"a\nb\nc\ntail".as_slice()),
            (
                b"tail\r\n".as_slice(),
                b"a\r\nb\r\nc\r\ntail\r\n".as_slice(),
            ),
        ] {
            let (_directory, path, mut editor) = fixture(source);
            editor.paste("a\rb\r\nc\n");
            assert_eq!(editor.lines()[..3], ["a", "b", "c"]);
            editor.save().unwrap();
            assert_eq!(fs::read(path).unwrap(), expected);
        }
    }

    #[test]
    fn empty_private_clipboard_paste_preserves_selection_and_undo_state() {
        let (_directory, _path, mut editor) = fixture(b"selected");
        ctrl(&mut editor, 'a');
        let selection = editor.selection();
        let cursor = editor.cursor();
        let revision = editor.revision();
        ctrl(&mut editor, 'v');
        assert_eq!(editor.lines(), &["selected"]);
        assert_eq!(editor.selection(), selection);
        assert_eq!(editor.cursor(), cursor);
        assert_eq!(editor.revision(), revision);
        assert!(!editor.dirty());
        ctrl(&mut editor, 'z');
        assert_eq!(editor.selection(), selection);
        assert_eq!(editor.lines(), &["selected"]);
        assert!(!editor.dirty());
    }

    #[test]
    fn paste_rejects_controls_and_limits_without_partial_edits() {
        let (_directory, _path, mut editor) = fixture(b"safe");
        for invalid in ["bad\0text", "bad\u{1b}text", "bad\u{85}text"] {
            editor.paste(invalid);
            assert_eq!(editor.lines(), &["safe"]);
            assert!(!editor.dirty());
        }
        editor.paste(&"x".repeat(MAX_EDITOR_BYTES));
        assert_eq!(editor.lines(), &["safe"]);
        editor.paste(&"\n".repeat(MAX_EDITOR_LINES));
        assert_eq!(editor.lines(), &["safe"]);
        assert_eq!(editor.revision(), 0);
        editor.paste("a\r\nb\nc\t");
        assert_eq!(editor.lines(), &["a", "b", "c\tsafe"]);
    }

    #[test]
    fn source_size_and_line_limits_are_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source");
        fs::write(&path, vec![b'x'; MAX_EDITOR_BYTES + 1]).unwrap();
        assert!(Editor::open(path.clone(), 1).is_err());
        fs::write(&path, "\n".repeat(MAX_EDITOR_LINES)).unwrap();
        assert!(Editor::open(path.clone(), 1).is_err());
        fs::write(&path, "\n".repeat(MAX_EDITOR_LINES - 1)).unwrap();
        let editor = Editor::open(path, usize::MAX).unwrap();
        assert_eq!(editor.cursor(), (MAX_EDITOR_LINES - 1, 0));
    }

    #[test]
    fn save_rejects_symlink_or_hardlink_added_after_open() {
        let (directory, path, mut editor) = fixture(b"original");
        editor.paste("edit");
        let target = directory.path().join("target");
        fs::rename(&path, &target).unwrap();
        symlink(&target, &path).unwrap();
        assert!(editor.save().is_err());
        assert!(editor.dirty());
        assert_eq!(fs::read(&target).unwrap(), b"original");
        fs::remove_file(&path).unwrap();
        fs::rename(&target, &path).unwrap();
        fs::hard_link(&path, &target).unwrap();
        assert!(editor.save().is_err());
        assert_eq!(fs::read(target).unwrap(), b"original");
    }

    #[test]
    fn undo_deletion_restores_caret_without_a_synthetic_selection() {
        let (_directory, _path, mut editor) = fixture(b"abc");
        plain(&mut editor, KeyCode::End);
        plain(&mut editor, KeyCode::Backspace);
        ctrl(&mut editor, 'z');
        assert_eq!(editor.lines(), &["abc"]);
        assert_eq!(editor.cursor(), (0, 3));
        assert_eq!(editor.selection(), None);
        plain(&mut editor, KeyCode::Home);
        plain(&mut editor, KeyCode::Delete);
        ctrl(&mut editor, 'z');
        assert_eq!(editor.cursor(), (0, 0));
        assert_eq!(editor.selection(), None);
    }

    #[test]
    fn insertion_that_joins_graphemes_keeps_cursor_on_a_boundary() {
        let (_directory, _path, mut editor) = fixture("\u{301}z".as_bytes());
        editor.paste("e");
        assert_eq!(editor.cursor(), (0, "e\u{301}".len()));
        ctrl(&mut editor, 'z');
        assert_eq!(editor.lines(), &["\u{301}z"]);
        ctrl(&mut editor, 'y');
        plain(&mut editor, KeyCode::Backspace);
        assert_eq!(editor.lines(), &["z"]);
    }
}
