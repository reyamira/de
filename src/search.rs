//! Bounded, cancellable recursive name and UTF-8 source search.

use crate::matching::LiteralMatcher;
use ignore::WalkBuilder;
use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const MAX_CONTENT_BYTES: usize = 1024 * 1024;
const MAX_CONTENT_LINES: usize = 20_000;
// Synthetic file headers do not consume this limit.
const MAX_RESULTS: usize = 10_000;
const BATCH_SIZE: usize = 64;
const QUEUED_BATCHES: usize = 4;
const PROGRESS_INTERVAL: usize = 128;
const PROGRESS_LATENCY: Duration = Duration::from_millis(50);
const MAX_EXCERPT_CHARS: usize = 240;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SearchScope {
    #[default]
    Both,
    Names,
    Content,
}

impl SearchScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Both => "Both",
            Self::Names => "Files & folders",
            Self::Content => "Content",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Both => Self::Names,
            Self::Names => Self::Content,
            Self::Content => Self::Both,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum SearchHitKind {
    Name,
    File,
    Line,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchHit {
    pub kind: SearchHitKind,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// Original source line number, starting at one. File headers target their first match.
    pub line: Option<usize>,
    pub excerpt: String,
    /// First original UTF-8 match range, relative to the full source line.
    /// Name-hit ranges are relative to the displayed path instead.
    pub first_match: Range<usize>,
    /// Original UTF-8 byte offset of the excerpt within the source line.
    pub excerpt_start: usize,
}

#[derive(Debug)]
pub struct SearchBatch {
    pub generation: u64,
    pub hits: Vec<SearchHit>,
    pub done: bool,
    /// Cumulative number of visited paths, excluding the search root directory.
    pub scanned: usize,
    /// The result limit stopped this generation before traversal completed.
    pub truncated: bool,
}

#[derive(Debug)]
struct Request {
    generation: u64,
    root: PathBuf,
    query: String,
    show_hidden: bool,
    scope: SearchScope,
}

#[derive(Debug, Default)]
struct State {
    queued: Option<Request>,
    ready: VecDeque<SearchBatch>,
    pending: bool,
}

#[derive(Debug, Default)]
struct Shared {
    generation: AtomicU64,
    shutdown: AtomicBool,
    state: Mutex<State>,
    wake: Condvar,
    #[cfg(test)]
    source_reads: std::sync::atomic::AtomicUsize,
}

/// One background scanner with coalesced requests and bounded result storage.
#[derive(Debug)]
pub struct SearchWorker {
    shared: Arc<Shared>,
}

impl SearchWorker {
    pub fn new() -> Self {
        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        thread::spawn(move || worker_loop(worker));
        Self { shared }
    }

    /// Replace the current search. Relative path names and contents use lowercase matching.
    pub fn request(
        &mut self,
        root: PathBuf,
        query: String,
        show_hidden: bool,
        scope: SearchScope,
    ) -> u64 {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let generation = self
            .shared
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        state.queued = None;
        state.ready.clear();
        state.pending = !query.is_empty();
        if query.is_empty() {
            state.ready.push_back(SearchBatch {
                generation,
                hits: Vec::new(),
                done: true,
                scanned: 0,
                truncated: false,
            });
        } else {
            state.queued = Some(Request {
                generation,
                root,
                query,
                show_hidden,
                scope,
            });
        }
        self.shared.wake.notify_all();
        generation
    }

    pub fn cancel(&mut self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        state.queued = None;
        state.ready.clear();
        state.pending = false;
        self.shared.wake.notify_all();
    }

    pub fn poll(&mut self) -> Option<SearchBatch> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let generation = self.shared.generation.load(Ordering::Acquire);
        let result = loop {
            match state.ready.pop_front() {
                Some(batch) if batch.generation == generation => break Some(batch),
                Some(_) => continue,
                None => break None,
            }
        };
        if result.as_ref().is_some_and(|batch| batch.done) {
            state.pending = false;
        }
        self.shared.wake.notify_all();
        result
    }

    /// Nonempty requests stay pending until their final batch is consumed.
    pub fn is_pending(&self) -> bool {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
    }
}

impl Default for SearchWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SearchWorker {
    fn drop(&mut self) {
        let _state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        // Do not join: an in-flight filesystem operation must not delay the UI.
        self.shared.wake.notify_all();
    }
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        let request = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            while state.queued.is_none() && !shared.shutdown.load(Ordering::Acquire) {
                state = shared.wake.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            if shared.shutdown.load(Ordering::Acquire) {
                return;
            }
            state.queued.take().expect("queued search request")
        };
        search(&shared, request);
    }
}

struct SearchRun<'a> {
    shared: &'a Shared,
    generation: u64,
    hits: Vec<SearchHit>,
    total_hits: usize,
    scanned: usize,
    last_publish: Instant,
}

impl SearchRun<'_> {
    fn cancelled(&self) -> bool {
        self.shared.shutdown.load(Ordering::Acquire)
            || self.shared.generation.load(Ordering::Acquire) != self.generation
    }

    fn publish(&mut self, done: bool, truncated: bool) -> bool {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.ready.len() >= QUEUED_BATCHES && !self.cancelled() {
            state = self
                .shared
                .wake
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
        if self.cancelled() {
            return false;
        }
        let hits = std::mem::replace(
            &mut self.hits,
            if done {
                Vec::new()
            } else {
                Vec::with_capacity(BATCH_SIZE)
            },
        );
        state.ready.push_back(SearchBatch {
            generation: self.generation,
            hits,
            done,
            scanned: self.scanned,
            truncated,
        });
        self.last_publish = Instant::now();
        true
    }

    fn progress(&mut self) -> bool {
        !self.cancelled()
            && (self.last_publish.elapsed() < PROGRESS_LATENCY || self.publish(false, false))
    }

    /// Add one real match and its optional synthetic file header together.
    fn push(&mut self, hit: SearchHit, header: Option<SearchHit>) -> bool {
        if self.cancelled() {
            return false;
        }
        if header.is_some() && self.hits.len() + 2 > BATCH_SIZE && !self.publish(false, false) {
            return false;
        }
        if let Some(header) = header {
            self.hits.push(header);
        }
        self.hits.push(hit);
        self.total_hits += 1;
        if self.total_hits == MAX_RESULTS {
            self.publish(true, true);
            return false;
        }
        self.hits.len() < BATCH_SIZE || self.publish(false, false)
    }
}

fn search(shared: &Shared, request: Request) {
    let mut run = SearchRun {
        shared,
        generation: request.generation,
        hits: Vec::with_capacity(BATCH_SIZE),
        total_hits: 0,
        scanned: 0,
        last_publish: Instant::now(),
    };
    let matcher = LiteralMatcher::new(&request.query);

    // Only an explicitly selected directory root may resolve through a symlink.
    if let Ok(metadata) = std::fs::symlink_metadata(&request.root)
        && metadata.file_type().is_symlink()
        && !std::fs::metadata(&request.root).is_ok_and(|target| target.is_dir())
    {
        run.scanned = 1;
        if request.scope != SearchScope::Content {
            let name = request
                .root
                .file_name()
                .unwrap_or(request.root.as_os_str())
                .to_string_lossy()
                .into_owned();
            if let Some(first_match) = matcher.first(&name)
                && !run.push(
                    SearchHit {
                        kind: SearchHitKind::Name,
                        path: request.root,
                        is_dir: false,
                        is_symlink: true,
                        line: None,
                        excerpt: name,
                        first_match,
                        excerpt_start: 0,
                    },
                    None,
                )
            {
                return;
            }
        }
        run.publish(true, false);
        return;
    }
    let traversal_root = request
        .root
        .canonicalize()
        .unwrap_or_else(|_| request.root.clone());
    let mut builder = WalkBuilder::new(&traversal_root);
    builder
        .hidden(!request.show_hidden)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_name(|left, right| left.cmp(right));

    let mut walk = builder.build();
    loop {
        if run.cancelled() {
            return;
        }
        let Some(entry) = walk.next() else { break };
        let Ok(entry) = entry else { continue };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        let is_symlink = file_type.is_symlink() || entry.path_is_symlink();
        let is_dir = if is_symlink {
            std::fs::metadata(entry.path()).is_ok_and(|target| target.is_dir())
        } else {
            file_type.is_dir()
        };
        if entry.depth() == 0 && is_dir {
            continue;
        }
        run.scanned += 1;
        let relative = entry
            .path()
            .strip_prefix(&traversal_root)
            .unwrap_or(entry.path());
        let relative = if relative.as_os_str().is_empty() {
            Path::new(entry.file_name())
        } else {
            relative
        };
        let logical_path = || {
            if entry.depth() == 0 {
                request.root.clone()
            } else {
                request.root.join(relative)
            }
        };
        let mut has_header = false;
        if request.scope != SearchScope::Content {
            let name = relative.to_string_lossy();
            if let Some(first_match) = matcher.first(&name) {
                if !run.push(
                    SearchHit {
                        kind: SearchHitKind::Name,
                        path: logical_path(),
                        is_dir,
                        is_symlink,
                        line: None,
                        excerpt: name.into_owned(),
                        first_match,
                        excerpt_start: 0,
                    },
                    None,
                ) {
                    return;
                }
                has_header = true;
            }
        }
        if request.scope != SearchScope::Names
            && file_type.is_file()
            && !is_symlink
            && let Some(source) = read_source(entry.path(), &mut run)
        {
            // Validate the complete file before emitting any content hits.
            let mut line_count: usize = 0;
            for _ in crate::document::logical_lines(&source) {
                if run.cancelled() {
                    return;
                }
                line_count += 1;
                if line_count.is_multiple_of(PROGRESS_INTERVAL) && !run.progress() {
                    return;
                }
                if line_count > MAX_CONTENT_LINES {
                    break;
                }
            }
            if line_count <= MAX_CONTENT_LINES {
                for (index, line) in crate::document::logical_lines(&source).enumerate() {
                    if run.cancelled() {
                        return;
                    }
                    if index.is_multiple_of(PROGRESS_INTERVAL) && !run.progress() {
                        return;
                    }
                    if let Some(first_match) = matcher.first(line) {
                        let header = if has_header {
                            None
                        } else {
                            has_header = true;
                            Some(SearchHit {
                                kind: SearchHitKind::File,
                                path: logical_path(),
                                is_dir: false,
                                is_symlink: false,
                                line: Some(index + 1),
                                excerpt: String::new(),
                                first_match: first_match.clone(),
                                excerpt_start: 0,
                            })
                        };
                        let (excerpt, excerpt_start) = excerpt(line, first_match.start);
                        if !run.push(
                            SearchHit {
                                kind: SearchHitKind::Line,
                                path: logical_path(),
                                is_dir: false,
                                is_symlink: false,
                                line: Some(index + 1),
                                excerpt,
                                first_match,
                                excerpt_start,
                            },
                            header,
                        ) {
                            return;
                        }
                    }
                }
            }
        }
        if run.scanned.is_multiple_of(PROGRESS_INTERVAL) {
            if !run.publish(false, false) {
                return;
            }
        } else if !run.progress() {
            return;
        }
    }
    run.publish(true, false);
}

fn read_source(path: &Path, run: &mut SearchRun<'_>) -> Option<String> {
    use rustix::fs::{Mode, OFlags, open, openat};

    #[cfg(test)]
    run.shared.source_reads.fetch_add(1, Ordering::Relaxed);

    // Open every path component without following symlinks, including parent directories.
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = open(
        if path.is_absolute() { "/" } else { "." },
        directory_flags,
        Mode::empty(),
    )
    .ok()?;
    let mut components = path
        .components()
        .filter(|component| !matches!(component, Component::RootDir | Component::CurDir))
        .peekable();
    let mut file = loop {
        if !run.progress() {
            return None;
        }
        let component = components.next()?;
        if components.peek().is_some() {
            directory = openat(
                &directory,
                component.as_os_str(),
                directory_flags,
                Mode::empty(),
            )
            .ok()?;
        } else {
            let fd = openat(
                &directory,
                component.as_os_str(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .ok()?;
            break File::from(fd);
        }
    };
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_CONTENT_BYTES as u64 {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    let mut chunk = [0; 8192];
    loop {
        if !run.progress() {
            return None;
        }
        let remaining = (MAX_CONTENT_BYTES + 1 - bytes.len()).min(chunk.len());
        let count = match file.read(&mut chunk[..remaining]) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        if chunk[..count].contains(&0) || bytes.len() + count > MAX_CONTENT_BYTES {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    String::from_utf8(bytes).ok()
}

fn excerpt(line: &str, first_match: usize) -> (String, usize) {
    // Keep some leading context, but never hide the first match behind an
    // arbitrary prefix. No leading marker is added: excerpt offsets remain
    // directly relative to the original source bytes.
    let context_bytes: usize = line[..first_match]
        .chars()
        .rev()
        .take(MAX_EXCERPT_CHARS / 4)
        .map(char::len_utf8)
        .sum();
    // Remove only indentation before the match, including for whitespace queries.
    let indentation = first_match - line[..first_match].trim_start().len();
    let start = (first_match - context_bytes).max(indentation);
    let window = &line[start..];
    let excerpt = match window.char_indices().nth(MAX_EXCERPT_CHARS) {
        Some((end, _)) => {
            let mut excerpt = String::from(&window[..end]);
            excerpt.push('…');
            excerpt
        }
        None => window.to_owned(),
    };
    (excerpt, start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    fn batches(worker: &mut SearchWorker, generation: u64) -> Vec<SearchBatch> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut result = Vec::new();
        loop {
            if let Some(batch) = worker.poll() {
                assert_eq!(batch.generation, generation, "stale search result");
                assert!(batch.hits.len() <= BATCH_SIZE);
                let done = batch.done;
                result.push(batch);
                if done {
                    assert!(!worker.is_pending());
                    return result;
                }
            } else {
                assert!(Instant::now() < deadline, "search did not finish");
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn hits(worker: &mut SearchWorker, root: &Path, query: &str, hidden: bool) -> Vec<SearchHit> {
        scoped_hits(worker, root, query, hidden, SearchScope::Both)
    }

    fn scoped_hits(
        worker: &mut SearchWorker,
        root: &Path,
        query: &str,
        hidden: bool,
        scope: SearchScope,
    ) -> Vec<SearchHit> {
        let generation = worker.request(root.to_path_buf(), query.to_owned(), hidden, scope);
        batches(worker, generation)
            .into_iter()
            .flat_map(|batch| batch.hits)
            .collect()
    }

    #[test]
    fn filters_scopes_and_never_reads_sources_for_names() {
        use SearchHitKind::{File, Line, Name};

        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("needle-folder")).unwrap();
        fs::write(root.path().join("needle-folder/child.txt"), "plain\n").unwrap();
        fs::write(root.path().join("needle.txt"), "plain\nneedle\nneedle\n").unwrap();
        fs::write(root.path().join("text.txt"), "needle\n").unwrap();
        let mut worker = SearchWorker::new();
        let names = scoped_hits(
            &mut worker,
            root.path(),
            "needle",
            false,
            SearchScope::Names,
        );
        assert_eq!(worker.shared.source_reads.load(Ordering::Relaxed), 0);
        assert_eq!(names.len(), 3);
        assert!(
            names
                .iter()
                .all(|hit| hit.kind == Name && hit.line.is_none())
        );
        assert_eq!(names[1].path, root.path().join("needle-folder/child.txt"));

        let content = scoped_hits(
            &mut worker,
            root.path(),
            "needle",
            false,
            SearchScope::Content,
        );
        assert_eq!(worker.shared.source_reads.load(Ordering::Relaxed), 3);
        let expected = [
            ("needle.txt", File, Some(2)),
            ("needle.txt", Line, Some(2)),
            ("needle.txt", Line, Some(3)),
            ("text.txt", File, Some(1)),
            ("text.txt", Line, Some(1)),
        ];
        assert_eq!(content.len(), expected.len());
        for (hit, (path, kind, line)) in content.iter().zip(expected) {
            assert_eq!(hit.path, root.path().join(path));
            assert_eq!(hit.kind, kind);
            assert_eq!(hit.line, line);
        }

        let both = hits(&mut worker, root.path(), "needle", false);
        let expected = [
            ("needle-folder", Name, None),
            ("needle-folder/child.txt", Name, None),
            ("needle.txt", Name, None),
            ("needle.txt", Line, Some(2)),
            ("needle.txt", Line, Some(3)),
            ("text.txt", File, Some(1)),
            ("text.txt", Line, Some(1)),
        ];
        assert_eq!(both.len(), expected.len());
        for (hit, (path, kind, line)) in both.iter().zip(expected) {
            assert_eq!(hit.path, root.path().join(path));
            assert_eq!(hit.kind, kind);
            assert_eq!(hit.line, line);
        }
    }

    #[test]
    fn combines_relative_names_and_original_source_lines_in_path_order() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("Needle")).unwrap();
        fs::write(
            root.path().join("Needle/child.txt"),
            "plain\nβ NEEDLE café\r\nneedle needle\n",
        )
        .unwrap();
        fs::write(root.path().join("Needle.txt"), "NeEdLe\n").unwrap();
        let result = hits(&mut SearchWorker::new(), root.path(), "nEeDlE", false);
        let expected = [
            ("Needle", None, "Needle"),
            ("Needle/child.txt", None, "Needle/child.txt"),
            ("Needle/child.txt", Some(2), "β NEEDLE café"),
            ("Needle/child.txt", Some(3), "needle needle"),
            ("Needle.txt", None, "Needle.txt"),
            ("Needle.txt", Some(1), "NeEdLe"),
        ];
        assert_eq!(result.len(), expected.len());
        for (hit, (path, line, excerpt)) in result.iter().zip(expected) {
            assert_eq!(hit.path, root.path().join(path));
            assert_eq!(hit.line, line);
            assert_eq!(
                hit.kind,
                if line.is_none() {
                    SearchHitKind::Name
                } else {
                    SearchHitKind::Line
                }
            );
            assert_eq!(hit.excerpt, excerpt);
            assert_eq!(hit.is_dir, path == "Needle");
            assert!(!hit.is_symlink);
        }
        assert!(result.windows(2).all(|pair| {
            (&pair[0].path, pair[0].kind, pair[0].line)
                < (&pair[1].path, pair[1].kind, pair[1].line)
        }));
    }

    #[test]
    fn matches_literals_and_unicode_without_changing_source() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("text.txt"),
            "literal [A.*] ÉCLAIR\nregex Axxx\n",
        )
        .unwrap();
        let mut worker = SearchWorker::new();
        let result = hits(&mut worker, root.path(), "[a.*]", false);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].kind, SearchHitKind::File);
        assert!(result[0].excerpt.is_empty());
        assert_eq!(result[1].excerpt, "literal [A.*] ÉCLAIR");
        assert_eq!(hits(&mut worker, root.path(), "éclair", false).len(), 2);
    }

    #[test]
    fn keeps_original_first_match_for_paths_and_lowercase_expansions() {
        let root = tempdir().unwrap();
        let path = root.path().join("İ-İ.txt");
        fs::write(&path, "\u{3000}\tβ İ i\u{307} İ\r\n").unwrap();
        let result = hits(&mut SearchWorker::new(), root.path(), "i", false);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].path, path);
        assert_eq!(result[0].line, None);
        assert_eq!(result[0].excerpt, "İ-İ.txt");
        assert_eq!(result[0].excerpt_start, 0);
        assert_eq!(result[0].first_match, 0..2);
        assert_eq!(result[1].line, Some(1));
        assert_eq!(result[1].excerpt, "β İ i\u{307} İ");
        assert_eq!(result[1].excerpt_start, 4);
        assert_eq!(result[1].first_match, 7..9);
        let content = scoped_hits(
            &mut SearchWorker::new(),
            root.path(),
            "i",
            false,
            SearchScope::Content,
        );
        assert_eq!(content.len(), 2);
        assert_eq!(content[0].kind, SearchHitKind::File);
        assert_eq!(content[0].path, path);
        assert_eq!(content[0].line, Some(1));
        assert_eq!(content[0].first_match, 7..9);
        assert_eq!(content[0].excerpt_start, 0);
        assert!(content[0].excerpt.is_empty());
        assert_eq!(content[1], result[1]);
        let whitespace = scoped_hits(
            &mut SearchWorker::new(),
            root.path(),
            "\t",
            false,
            SearchScope::Content,
        );
        assert_eq!(whitespace.len(), 2);
        assert_eq!(whitespace[0].first_match, 3..4);
        assert_eq!(whitespace[1].first_match, 3..4);
        assert_eq!(whitespace[1].excerpt_start, 3);
        assert_eq!(whitespace[1].excerpt, "\tβ İ i\u{307} İ");
    }

    #[test]
    fn long_line_windows_keep_the_original_first_match() {
        let root = tempdir().unwrap();
        let line = format!("{}NEEDLE{}needle", "é".repeat(300), "β".repeat(300));
        fs::write(root.path().join("text.txt"), format!("plain\r\n{line}\r\n")).unwrap();
        let result = hits(&mut SearchWorker::new(), root.path(), "needle", false);
        assert_eq!(result.len(), 2);
        let hit = &result[1];
        assert_eq!(hit.line, Some(2));
        assert_eq!(hit.excerpt_start, 480);
        assert_eq!(hit.first_match, 600..606);
        assert!(hit.excerpt.contains("NEEDLE"));
        assert!(!hit.excerpt.contains("needle"));
        assert_eq!(hit.excerpt.chars().count(), MAX_EXCERPT_CHARS + 1);
        let visible = &hit.first_match;
        assert_eq!(
            &hit.excerpt[visible.start - hit.excerpt_start..visible.end - hit.excerpt_start],
            &line[visible.clone()]
        );
    }

    #[test]
    fn respects_hidden_and_ignore_files_outside_git() {
        let root = tempdir().unwrap();
        fs::write(root.path().join(".gitignore"), "git-ignored.txt\n").unwrap();
        fs::write(root.path().join(".ignore"), "other-ignored.txt\n").unwrap();
        for name in [
            "visible.txt",
            ".hidden.txt",
            "git-ignored.txt",
            "other-ignored.txt",
        ] {
            fs::write(root.path().join(name), "needle\n").unwrap();
        }
        let mut worker = SearchWorker::new();
        let visible = hits(&mut worker, root.path(), "needle", false);
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].path, root.path().join("visible.txt"));
        let hidden = hits(&mut worker, root.path(), "needle", true);
        assert_eq!(hidden.len(), 4);
        assert!(hidden.iter().any(|hit| hit.path.ends_with(".hidden.txt")));
        assert!(
            hidden
                .iter()
                .all(|hit| !hit.path.ends_with("git-ignored.txt")
                    && !hit.path.ends_with("other-ignored.txt"))
        );
    }

    #[test]
    fn skips_binary_invalid_utf8_oversized_and_overlong_contents_but_keeps_names() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("needle-binary"), b"needle\0needle\n").unwrap();
        fs::write(root.path().join("needle-invalid"), b"needle\n\xff").unwrap();
        fs::write(
            root.path().join("needle-large"),
            "needle".repeat(MAX_CONTENT_BYTES / 6 + 1),
        )
        .unwrap();
        fs::write(
            root.path().join("needle-lines"),
            "needle\n".repeat(MAX_CONTENT_LINES + 1),
        )
        .unwrap();
        fs::write(root.path().join("valid.txt"), "needle\n").unwrap();
        let result = hits(&mut SearchWorker::new(), root.path(), "needle", false);
        assert_eq!(result.len(), 6);
        assert_eq!(
            result
                .iter()
                .filter(|hit| hit.kind == SearchHitKind::Name)
                .count(),
            4
        );
        assert_eq!(
            result
                .iter()
                .filter(|hit| hit.kind == SearchHitKind::Line)
                .count(),
            1
        );
        assert_eq!(
            result
                .iter()
                .find(|hit| hit.kind == SearchHitKind::Line)
                .unwrap()
                .path,
            root.path().join("valid.txt")
        );
    }

    #[cfg(unix)]
    #[test]
    fn reports_symlink_names_without_traversal_or_content_reads() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("needle-child"), "needle\n").unwrap();
        symlink(outside.path(), root.path().join("needle-dir")).unwrap();
        symlink(
            outside.path().join("needle-child"),
            root.path().join("needle-file"),
        )
        .unwrap();
        symlink(
            outside.path().join("missing"),
            root.path().join("needle-dangling"),
        )
        .unwrap();
        let mut worker = SearchWorker::new();
        let result = hits(&mut worker, root.path(), "needle", false);
        assert_eq!(result.len(), 3);
        assert!(
            result
                .iter()
                .all(|hit| hit.is_symlink && hit.line.is_none())
        );
        assert!(
            result
                .iter()
                .find(|hit| hit.path.ends_with("needle-dir"))
                .unwrap()
                .is_dir
        );
        assert!(
            result
                .iter()
                .filter(|hit| !hit.path.ends_with("needle-dir"))
                .all(|hit| !hit.is_dir)
        );

        // Resolve the selected root only. The nested link must remain a name hit.
        symlink(root.path(), outside.path().join("needle-nested")).unwrap();
        let linked_root = root.path().join("needle-dir");
        let result = hits(&mut worker, &linked_root, "needle", false);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].path, linked_root.join("needle-child"));
        assert_eq!(result[0].line, None);
        assert_eq!(result[1].path, linked_root.join("needle-child"));
        assert_eq!(result[1].line, Some(1));
        assert_eq!(result[2].path, linked_root.join("needle-nested"));
        assert!(result[2].is_symlink && result[2].is_dir && result[2].line.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn searches_logical_root_with_symlink_ancestor_and_preserves_logical_paths() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::create_dir(outside.path().join("folder")).unwrap();
        fs::write(outside.path().join("folder/text.txt"), "needle\n").unwrap();
        symlink(outside.path(), root.path().join("alias")).unwrap();
        let logical_root = root.path().join("alias/folder");
        let result = hits(&mut SearchWorker::new(), &logical_root, "needle", false);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].path, logical_root.join("text.txt"));
        assert_eq!(result[0].line, Some(1));
        assert!(!result[0].is_symlink);
    }

    #[test]
    fn replaces_backpressured_scope_generations_and_cancel_clears_pending_results() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("old.txt"),
            "old\n".repeat(BATCH_SIZE * (QUEUED_BATCHES + 2)),
        )
        .unwrap();
        fs::write(root.path().join("new.txt"), "new result\n").unwrap();
        let mut worker = SearchWorker::new();
        let stale = worker.request(
            root.path().to_path_buf(),
            "old".to_owned(),
            false,
            SearchScope::Content,
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while worker.shared.state.lock().unwrap().ready.len() < QUEUED_BATCHES {
            assert!(
                Instant::now() < deadline,
                "old generation did not fill its queue"
            );
            thread::sleep(Duration::from_millis(1));
        }
        let current = worker.request(
            root.path().to_path_buf(),
            "old".to_owned(),
            false,
            SearchScope::Names,
        );
        assert_ne!(stale, current);
        let result: Vec<_> = batches(&mut worker, current)
            .into_iter()
            .flat_map(|batch| batch.hits)
            .collect();
        assert_eq!(result.len(), 1);
        assert!(result[0].path.ends_with("old.txt"));
        assert_eq!(result[0].kind, SearchHitKind::Name);
        worker.request(
            root.path().to_path_buf(),
            "old".to_owned(),
            false,
            SearchScope::Content,
        );
        worker.cancel();
        assert!(!worker.is_pending());
        assert!(worker.poll().is_none());
        let current = worker.request(
            root.path().to_path_buf(),
            "new".to_owned(),
            false,
            SearchScope::Content,
        );
        assert!(batches(&mut worker, current).last().unwrap().done);
    }

    #[test]
    fn empty_query_completes_immediately_without_traversal() {
        let mut worker = SearchWorker::new();
        let generation = worker.request(
            PathBuf::from("/nonexistent/search/root"),
            String::new(),
            false,
            SearchScope::Both,
        );
        assert!(!worker.is_pending());
        let batch = worker.poll().unwrap();
        assert_eq!(batch.generation, generation);
        assert!(batch.done && batch.hits.is_empty() && !batch.truncated);
        assert_eq!(batch.scanned, 0);
        assert!(worker.poll().is_none());
    }

    #[test]
    fn bounds_real_matches_without_orphaning_file_headers() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("needle-a.txt"),
            "needle\n".repeat(MAX_RESULTS - 1),
        )
        .unwrap();
        fs::write(root.path().join("needle-b.txt"), "needle\nneedle\n").unwrap();
        let mut worker = SearchWorker::new();
        for scope in [SearchScope::Both, SearchScope::Content] {
            let generation =
                worker.request(root.path().to_path_buf(), "needle".to_owned(), false, scope);
            let result = batches(&mut worker, generation);
            let all: Vec<_> = result.iter().flat_map(|batch| &batch.hits).collect();
            assert_eq!(
                all.iter()
                    .filter(|hit| hit.kind != SearchHitKind::File)
                    .count(),
                MAX_RESULTS
            );
            assert!(result.last().unwrap().truncated);
            assert!(
                result
                    .windows(2)
                    .all(|pair| pair[0].scanned <= pair[1].scanned)
            );
            if scope == SearchScope::Content {
                assert_eq!(all.len(), MAX_RESULTS + 2);
                assert!(all.iter().all(|hit| hit.kind != SearchHitKind::Name));
                assert_eq!(result.last().unwrap().scanned, 2);
                assert_eq!(all[all.len() - 2].kind, SearchHitKind::File);
                assert_eq!(all.last().unwrap().kind, SearchHitKind::Line);
                assert_eq!(all.last().unwrap().line, Some(1));
                assert!(all.last().unwrap().path.ends_with("needle-b.txt"));
                for batch in &result {
                    for (index, hit) in batch.hits.iter().enumerate() {
                        if hit.kind == SearchHitKind::File {
                            let line = &batch.hits[index + 1];
                            assert_eq!(line.kind, SearchHitKind::Line);
                            assert_eq!(line.path, hit.path);
                            assert_eq!(line.line, hit.line);
                            assert_eq!(line.first_match, hit.first_match);
                        }
                    }
                }
            } else {
                assert_eq!(all.len(), MAX_RESULTS);
                assert_eq!(all[0].kind, SearchHitKind::Name);
                assert_eq!(result.last().unwrap().scanned, 1);
            }
        }
    }

    #[test]
    fn content_limits_are_inclusive() {
        let root = tempdir().unwrap();
        let mut exact_bytes = String::from("needle");
        exact_bytes.extend(std::iter::repeat_n(
            ' ',
            MAX_CONTENT_BYTES - exact_bytes.len(),
        ));
        fs::write(root.path().join("bytes.txt"), exact_bytes).unwrap();
        let mut exact_lines = "plain\n".repeat(MAX_CONTENT_LINES - 1);
        exact_lines.push_str("needle");
        fs::write(root.path().join("lines.txt"), exact_lines).unwrap();
        let result = hits(&mut SearchWorker::new(), root.path(), "needle", false);
        assert_eq!(result.len(), 4);
        assert_eq!(result[1].kind, SearchHitKind::Line);
        assert_eq!(result[1].line, Some(1));
        assert_eq!(result[3].kind, SearchHitKind::Line);
        assert_eq!(result[3].line, Some(MAX_CONTENT_LINES));
    }

    #[test]
    fn mixed_source_line_endings_and_eof_match_the_source_viewer() {
        let root = tempdir().unwrap();
        let path = root.path().join("mixed.txt");
        fs::write(&path, "a\rb\nneedle\r\nneedle\r").unwrap();
        let result = hits(&mut SearchWorker::new(), root.path(), "needle", false);
        assert_eq!(
            result
                .iter()
                .filter(|hit| hit.kind == SearchHitKind::Line)
                .map(|hit| hit.line.unwrap())
                .collect::<Vec<_>>(),
            [3, 4]
        );
        let document =
            crate::document::load_source(&path, &crate::Theme::auto().palette()).unwrap();
        assert_eq!(document.lines.len(), 5);
        for hit in result.iter().filter(|hit| hit.kind == SearchHitKind::Line) {
            assert_eq!(
                document.lines[hit.line.unwrap() - 1].to_string(),
                hit.excerpt
            );
        }
        let mut exact = "plain\r\n".repeat(MAX_CONTENT_LINES - 1);
        exact.push_str("needle");
        fs::write(&path, &exact).unwrap();
        assert_eq!(
            hits(&mut SearchWorker::new(), root.path(), "needle", false)[0].line,
            Some(MAX_CONTENT_LINES)
        );
        exact.push('\r');
        fs::write(&path, exact).unwrap();
        assert!(hits(&mut SearchWorker::new(), root.path(), "needle", false).is_empty());
    }

    #[test]
    fn nonempty_search_stays_pending_until_its_final_batch_is_consumed() {
        let root = tempdir().unwrap();
        let mut worker = SearchWorker::new();
        let generation = worker.request(
            root.path().to_path_buf(),
            "absent".to_owned(),
            false,
            SearchScope::Both,
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !worker
            .shared
            .state
            .lock()
            .unwrap()
            .ready
            .iter()
            .any(|batch| batch.done)
        {
            assert!(Instant::now() < deadline, "final batch was not queued");
            thread::sleep(Duration::from_millis(1));
        }
        assert!(worker.is_pending());
        let result = batches(&mut worker, generation);
        assert!(result.last().unwrap().hits.is_empty());
        assert!(!worker.is_pending());
    }

    #[test]
    fn publishes_scan_only_progress_after_the_latency_interval() {
        let shared = Shared::default();
        let mut run = SearchRun {
            shared: &shared,
            generation: 0,
            hits: Vec::new(),
            total_hits: 0,
            scanned: 17,
            last_publish: Instant::now() - PROGRESS_LATENCY,
        };
        assert!(run.progress());
        let state = shared.state.lock().unwrap();
        let batch = state.ready.front().unwrap();
        assert_eq!(batch.scanned, 17);
        assert!(batch.hits.is_empty());
        assert!(!batch.done && !batch.truncated);
    }

    #[test]
    fn excerpts_keep_utf8_boundaries_and_include_late_matches() {
        assert_eq!(excerpt("β café", 3), ("β café".to_owned(), 0));
        let source = "é".repeat(MAX_EXCERPT_CHARS + 1);
        assert_eq!(
            excerpt(&source, 0),
            (format!("{}…", "é".repeat(MAX_EXCERPT_CHARS)), 0)
        );
        let source = format!("{}NEEDLE{}", "é".repeat(300), "β".repeat(300));
        let (window, start) = excerpt(&source, 600);
        assert_eq!(start, 480);
        assert_eq!(window.chars().count(), MAX_EXCERPT_CHARS + 1);
        assert_eq!(&window[120..126], "NEEDLE");
        let visible_bytes = window.len() - '…'.len_utf8();
        assert_eq!(
            &source[start..start + visible_bytes],
            &window[..visible_bytes]
        );
        assert!(window.ends_with('…'));
        assert_eq!(excerpt("\u{3000}\tβ İ", 7), ("β İ".to_owned(), 4));
        assert_eq!(excerpt("    needle", 2), ("  needle".to_owned(), 2));
        assert_eq!(excerpt("    needle", 0), ("    needle".to_owned(), 0));
    }
}
