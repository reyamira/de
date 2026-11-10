//! Clean-room Markdown parser: line-based block parsing + a single-pass inline
//! parser. This is a focused CommonMark + GFM subset covering the constructs
//! that matter for documents (headings, paragraphs, fenced code, blockquotes,
//! lists + task lists, pipe tables, thematic breaks; emphasis/strong/strike,
//! code spans, links, images, autolinks, hard/soft breaks).
//!
//! It is deliberately not (yet) a full CommonMark implementation — full
//! reference conformance (remaining nested-list edge cases and HTML blocks) is
//! tracked in beads. The design priority is correct, fast handling of the
//! common 95% with zero dependencies and no `unwrap`/`panic`.

#![allow(dead_code, unexpected_cfgs, unused_imports)]
use std::collections::{BTreeMap, HashMap};
mod footnotes;
mod provenance;
mod safe_html;
mod toc;
pub(crate) use provenance::{
    Provenance, image_fallback, parse as parse_sourced, prepare_endnotes, with_assets,
};
use std::ops::Range;

use franken_markdown::ast::{Align, Block, Document, Inline, List, ListItem, Table};
use franken_markdown::scanner::{ParserLineScan, scan_markdown_line};
use franken_markdown::span::{ParseDiagnostic, SourceSpan, Spanned, SpannedDocument};

pub(crate) mod entities;
mod frontmatter;
mod html_blocks;
mod unicode_casefold;
mod unicode_punct;

pub use frontmatter::{Frontmatter, split_frontmatter};

mod destinations;
mod reference_labels;
pub use destinations::DestinationSpan;

pub use reference_labels::{ReferenceLabelForm, ReferenceLabelSpan};

#[cfg(not(target_arch = "wasm32"))]
type ParseStageStart = std::time::Instant;
#[cfg(target_arch = "wasm32")]
type ParseStageStart = ();

#[derive(Debug, Clone)]
struct LinkReference {
    dest: String,
    title: Option<String>,
}

type ReferenceMap = HashMap<String, LinkReference>;
type ConsumedReferenceLines = Vec<Range<usize>>;

const INLINE_PARSE_NOTES: &str =
    "parse inline delimiters, links, references, autolinks, code spans, and text";

const INLINE_PARSE_CACHE_MIN_BYTES: usize = 16;
const INLINE_PARSE_CACHE_MAX_KEY_BYTES: usize = 4096;
const INLINE_PARSE_CACHE_MAX_ENTRIES: usize = 512;
const INLINE_PARSE_CACHE_MAX_TOTAL_KEY_BYTES: usize = 128 * 1024;
/// Cap on distinct first-sighting hashes tracked for two-hit admission. When
/// reached, the tracker clears wholesale so late repeats in long documents
/// can still qualify after a fresh pair of sightings.
const INLINE_PARSE_CACHE_MAX_TRACKED_HASHES: usize = INLINE_PARSE_CACHE_MAX_ENTRIES;

/// Link/image labels recurse during tokenization, before the
/// separate emphasis-tree depth limit can apply. Bound that recursion and all
/// speculative inline scans within one run: rejected outer links can reparse
/// labels exponentially, while unmatched delimiters, HTML and destinations can
/// otherwise rescan overlapping suffixes quadratically.
const MAX_INLINE_PARSE_RECURSION: usize = 64;
const INLINE_PARSE_WORK_MULTIPLIER: usize = 64;

#[derive(Default)]
struct InlineWorkBudget {
    depth: usize,
    remaining: usize,
    exhausted: bool,
}

impl InlineWorkBudget {
    /// Charge actual inspected characters, including failed speculative parses.
    /// A scanner may finish one linear pass before charging it, so exhaustion
    /// costs at most one extra input-length scan. The outermost inline parser
    /// then preserves the complete source and discards speculative destinations.
    fn charge(&mut self, scanned: usize) -> bool {
        if self.exhausted || scanned > self.remaining {
            self.exhausted = true;
            return false;
        }
        self.remaining -= scanned;
        true
    }
}

#[derive(Default)]
struct InlineParseCache {
    entries: BTreeMap<String, Vec<Inline>>,
    total_key_bytes: usize,
    // Two-hit admission: most documents contain few repeated paragraphs, so a
    // first sighting would otherwise pay the insert-side deep clone of the
    // parsed tree (key `to_string` + `inlines.to_vec()`) for a hit that never
    // comes. `entries` therefore only unlocks for a text whose FNV-1a-64 hash
    // was already seen once in this document; first sightings are recorded in
    // this bounded flat hash list (no per-text allocation, no key retention).
    // Hash collisions can only cause a spurious admission (one wasted insert),
    // never a wrong lookup, because `entries` stays keyed by the exact text.
    seen_hashes: Vec<u64>,
}

impl InlineParseCache {
    fn get(&self, text: &str) -> Option<Vec<Inline>> {
        self.entries.get(text).cloned()
    }

    /// Two-hit admission gate: `false` on a text's first sighting (the parse
    /// result must not be stored yet), `true` from the second sighting on,
    /// which makes the caller's `insert` eligible. The second sighting still
    /// parses — by the time a repeat is known, the first tree is gone — so the
    /// earliest possible hit moves to the third sighting.
    fn admit(&mut self, text: &str) -> bool {
        let hash = inline_cache_text_hash(text);
        if self.seen_hashes.contains(&hash) {
            return true;
        }
        if self.seen_hashes.len() >= INLINE_PARSE_CACHE_MAX_TRACKED_HASHES {
            self.seen_hashes.clear();
        }
        self.seen_hashes.push(hash);
        false
    }

    fn insert(&mut self, text: &str, inlines: &[Inline]) {
        if self.entries.contains_key(text) {
            return;
        }
        if self.entries.len() >= INLINE_PARSE_CACHE_MAX_ENTRIES
            || self.total_key_bytes.saturating_add(text.len())
                > INLINE_PARSE_CACHE_MAX_TOTAL_KEY_BYTES
        {
            return;
        }
        self.total_key_bytes += text.len();
        self.entries.insert(text.to_string(), inlines.to_vec());
    }
}

#[inline(always)]
fn inline_cache_size_allows(text: &str) -> bool {
    text.len() >= INLINE_PARSE_CACHE_MIN_BYTES && text.len() <= INLINE_PARSE_CACHE_MAX_KEY_BYTES
}

/// FNV-1a 64-bit admission hash — the same construction used for content
/// fingerprints elsewhere in the crate (batch, compress, verify). Used for
/// admission only: a collision costs one pointless insert, never a false hit.
#[inline(always)]
fn inline_cache_text_hash(text: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for &byte in text.as_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// One measured parser stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseStageSummary {
    /// Stable stage identifier used by perf artifacts and Beads closeouts.
    pub stage: &'static str,
    /// Stage-specific work count: lines, blocks, inline input bytes, rows, etc.
    pub count: usize,
    /// Elapsed nanoseconds for this invocation. Zero on wasm32 until a browser
    /// clock provider exists.
    pub elapsed_ns: u128,
    /// Stage-specific input byte count when meaningful.
    pub bytes: usize,
    /// Approximate number of parser-owned objects/strings/vectors produced.
    pub allocations: usize,
    /// Short stable explanation for artifact readers.
    pub notes: &'static str,
}

struct ParseProfiler {
    enabled: bool,
    stages: Vec<ParseStageSummary>,
    // Reuse only outer inline handoffs. Recursive link/image/strike text is
    // parsed in place on the parent's char buffer and participates in
    // link-containment checks, so it intentionally bypasses this per-parse
    // cache (admission stays depth-0 only).
    inline_cache: InlineParseCache,
    inline_parse_depth: usize,
    inline_work: InlineWorkBudget,
    // GFM table escapes also apply inside code spans. Keep the original cell
    // source for destination tracking, and scope this decoding to its inlines.
    table_cell_code_escapes: bool,
    /// Current block-nesting recursion depth, used to bound deeply-nested
    /// blockquote/list input so pathological untrusted documents cannot overflow
    /// the stack (a DoS). Threaded for free since the profiler is already `&mut`
    /// through every block-parsing call.
    block_depth: usize,
    destinations: Option<destinations::DestinationTracker>,
    // Only the spanned entrypoint records top-level block consumption. The
    // normal parser remains the source of truth for every block boundary.
    block_spans: Option<BlockSpanTracker>,
}

impl ParseProfiler {
    fn disabled() -> Self {
        Self {
            enabled: false,
            stages: Vec::new(),
            inline_cache: InlineParseCache::default(),
            inline_parse_depth: 0,
            inline_work: InlineWorkBudget::default(),
            table_cell_code_escapes: false,
            block_depth: 0,
            destinations: None,
            block_spans: None,
        }
    }

    fn enabled() -> Self {
        Self {
            enabled: true,
            stages: Vec::new(),
            inline_cache: InlineParseCache::default(),
            inline_parse_depth: 0,
            inline_work: InlineWorkBudget::default(),
            table_cell_code_escapes: false,
            block_depth: 0,
            destinations: None,
            block_spans: None,
        }
    }

    fn checkpoint(&self) -> Option<ParseStageStart> {
        if self.enabled {
            parse_stage_now()
        } else {
            None
        }
    }

    fn record_since(
        &mut self,
        stage: &'static str,
        count: usize,
        bytes: usize,
        allocations: usize,
        notes: &'static str,
        started: Option<ParseStageStart>,
    ) {
        if !self.enabled {
            return;
        }
        self.stages.push(ParseStageSummary {
            stage,
            count,
            elapsed_ns: parse_stage_elapsed_ns(started),
            bytes,
            allocations,
            notes,
        });
    }

    fn measure<T, F, C>(&mut self, stage: &'static str, notes: &'static str, f: F, counts: C) -> T
    where
        F: FnOnce() -> T,
        C: FnOnce(&T) -> (usize, usize, usize),
    {
        let started = self.checkpoint();
        let result = f();
        let (count, bytes, allocations) = if self.enabled {
            counts(&result)
        } else {
            (0, 0, 0)
        };
        self.record_since(stage, count, bytes, allocations, notes, started);
        result
    }

    fn finish(self) -> Vec<ParseStageSummary> {
        self.stages
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_stage_now() -> Option<ParseStageStart> {
    Some(std::time::Instant::now())
}

#[cfg(target_arch = "wasm32")]
fn parse_stage_now() -> Option<ParseStageStart> {
    Some(())
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_stage_elapsed_ns(started: Option<ParseStageStart>) -> u128 {
    started.map_or(0, |start| start.elapsed().as_nanos())
}

#[cfg(target_arch = "wasm32")]
fn parse_stage_elapsed_ns(_started: Option<ParseStageStart>) -> u128 {
    0
}

/// Parse a full Markdown document.
#[must_use]
pub fn parse_document(src: &str) -> Document {
    let mut profiler = ParseProfiler::disabled();
    parse_document_inner(src, &mut profiler)
}

fn expand_leading_tabs_with_col(line: &str, start_col: usize) -> std::borrow::Cow<'_, str> {
    if !line.contains('\t') {
        return std::borrow::Cow::Borrowed(line);
    }
    let mut col = start_col;
    let mut expanded_prefix = String::new();
    let mut rest_idx = None;
    for (idx, ch) in line.char_indices() {
        match ch {
            ' ' => {
                col += 1;
                expanded_prefix.push(' ');
            }
            '\t' => {
                let spaces = 4 - (col % 4);
                col += spaces;
                for _ in 0..spaces {
                    expanded_prefix.push(' ');
                }
            }
            _ => {
                rest_idx = Some(idx);
                break;
            }
        }
    }
    let Some(idx) = rest_idx else {
        return std::borrow::Cow::Owned(expanded_prefix);
    };
    expanded_prefix.push_str(&line[idx..]);
    std::borrow::Cow::Owned(expanded_prefix)
}

fn expand_leading_tabs(line: &str) -> std::borrow::Cow<'_, str> {
    expand_leading_tabs_with_col(line, 0)
}

fn parse_document_inner(src: &str, profiler: &mut ParseProfiler) -> Document {
    // Ignore a leading BOM and recognize LF, CRLF, and lone CR without rewriting
    // the source buffer. Borrowed line slices retain original byte coordinates.
    // Frontmatter (key=value between --- fences at byte 0) is document
    // metadata, not content: split it off before line scanning so it never
    // renders as a thematic break or paragraph text.
    let src = document_body(src);
    let tab_storage: Option<Vec<String>> = if src.contains('\t') {
        Some(
            logical_lines(src)
                .map(|l| expand_leading_tabs(l).into_owned())
                .collect(),
        )
    } else {
        None
    };
    if let Some(storage) = &tab_storage {
        for (copied, original) in storage.iter().zip(logical_lines(src)) {
            profiler.destination_suffix(copied, original);
        }
    }
    let lines = profiler.measure(
        "line_split",
        "strip UTF-8 BOM if present and split source into logical lines",
        || {
            if let Some(storage) = &tab_storage {
                storage.iter().map(|s| s.as_str()).collect()
            } else {
                split_logical_lines(src)
            }
        },
        |lines| (lines.len(), src.len(), 1),
    );
    let reference_started = profiler.checkpoint();
    let has_reference_candidate = src.contains("]:");
    let (lines, refs, _, consumed_references) = if has_reference_candidate {
        collect_link_references_tracked(lines, profiler.destinations.as_mut())
    } else {
        (lines, ReferenceMap::new(), false, Vec::new())
    };
    let reference_allocations = refs.len() + usize::from(!consumed_references.is_empty());
    profiler.record_since(
        "reference_collection",
        refs.len(),
        src.len(),
        reference_allocations,
        "collect link reference definitions and preserve their source block boundaries",
        reference_started,
    );
    let block_started = profiler.checkpoint();
    let blocks =
        parse_blocks_with_reference_boundaries(&lines, &consumed_references, &refs, profiler);
    let block_count = blocks.len();
    profiler.record_since(
        "block_parse_total",
        block_count,
        src.len(),
        block_count,
        "line classification, block assembly, and recursive block parsing",
        block_started,
    );
    Document { blocks }
}

fn document_body(src: &str) -> &str {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    split_frontmatter(src).1
}

/// CommonMark 0.31.2 section 2.1: LF, lone CR, and CRLF each end one line.
/// Keep terminators in these borrowed slices so metadata can return an exact
/// body suffix; normal parsing strips them and inserts normalized AST breaks.
pub(crate) fn logical_lines_inclusive(mut source: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        if source.is_empty() {
            return None;
        }
        let end = source.find(['\r', '\n']).map_or(source.len(), |index| {
            index
                + if source.as_bytes()[index] == b'\r'
                    && source.as_bytes().get(index + 1) == Some(&b'\n')
                {
                    2
                } else {
                    1
                }
        });
        let (line, remaining) = source.split_at(end);
        source = remaining;
        Some(line)
    })
}

fn logical_line_content(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

fn logical_lines(source: &str) -> impl Iterator<Item = &str> {
    logical_lines_inclusive(source).map(logical_line_content)
}

fn split_logical_lines(src: &str) -> Vec<&str> {
    let mut lines = Vec::with_capacity(logical_line_capacity_hint(src.len()));
    lines.extend(logical_lines(src));
    lines
}

fn logical_line_capacity_hint(byte_len: usize) -> usize {
    if byte_len == 0 {
        0
    } else {
        byte_len.saturating_div(24).saturating_add(1).min(65_536)
    }
}

/// Parse a document and attach top-level source spans plus recoverable parser
/// diagnostics. This is intentionally additive: the normal renderer AST remains
/// span-free.
#[must_use]
pub fn parse_document_spanned(src: &str) -> SpannedDocument {
    parse_document_spanned_inner(src, &mut ParseProfiler::disabled())
}

fn parse_document_spanned_inner(src: &str, profiler: &mut ParseProfiler) -> SpannedDocument {
    let source_lines = source_lines(src);
    profiler.block_spans = Some(BlockSpanTracker {
        lines: source_lines
            .iter()
            .map(|line| SourceSpan::new(line.start, line.end))
            .collect(),
        blocks: Vec::new(),
    });
    let document = parse_document_inner(src, profiler);
    let span_started = profiler.checkpoint();
    let spans = profiler
        .block_spans
        .take()
        .map_or_else(Vec::new, |tracker| tracker.blocks);
    profiler.record_since(
        "span_collection",
        spans.len(),
        src.len(),
        spans.len(),
        "attach parser-consumed top-level source spans for editor/WASM diagnostics",
        span_started,
    );
    let fallback = SourceSpan::new(0, src.len());
    let blocks = document
        .blocks
        .into_iter()
        .enumerate()
        .map(|(idx, block)| Spanned::new(block, spans.get(idx).copied().unwrap_or(fallback)))
        .collect();

    SpannedDocument {
        blocks,
        diagnostics: profiler.measure(
            "diagnostics_collection",
            "collect recoverable parser diagnostics such as malformed references and fences",
            || collect_parse_diagnostics(src, &source_lines),
            |diagnostics| (diagnostics.len(), src.len(), diagnostics.len()),
        ),
        source_len: src.len(),
    }
}

#[derive(Debug, Clone, Copy)]
struct SourceLine<'a> {
    text: &'a str,
    start: usize,
    end: usize,
}

/// Original byte coordinates for the same logical lines the parser sees,
/// before tab expansion or reference-definition removal. Metadata never owns a
/// renderer block. All three line-ending forms retain their original byte widths.
fn source_lines(src: &str) -> Vec<SourceLine<'_>> {
    let body = document_body(src);
    let offset = src.len() - body.len();
    let address = body.as_ptr() as usize;
    logical_lines(body)
        .map(|text| {
            let start = offset + (text.as_ptr() as usize - address);
            SourceLine {
                text,
                start,
                end: start + text.len(),
            }
        })
        .collect()
}

struct BlockSpanTracker {
    /// One original span per top-level source line. Expanded tabs do not alter
    /// line count. Reference boundaries retain these coordinates, so each parsed
    /// segment records its original offset instead of joining unrelated blocks.
    lines: Vec<SourceSpan>,
    blocks: Vec<SourceSpan>,
}

impl BlockSpanTracker {
    fn record(&mut self, start: usize, end: usize) {
        self.blocks.push(span_for_lines(&self.lines, start, end));
    }
}

fn span_for_lines(lines: &[SourceSpan], start: usize, end: usize) -> SourceSpan {
    let Some(first) = lines.get(start) else {
        return SourceSpan::default();
    };
    let Some(last) = end.checked_sub(1).and_then(|idx| lines.get(idx)) else {
        return *first;
    };
    SourceSpan::new(first.start, last.end)
}

fn collect_parse_diagnostics(src: &str, lines: &[SourceLine<'_>]) -> Vec<ParseDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i];
        if looks_like_reference_definition(line.text)
            && parse_reference_definition(line.text).is_none()
        {
            diagnostics.push(ParseDiagnostic::warning(
                SourceSpan::new(line.start, line.end),
                "malformed link reference definition rendered as text",
            ));
        }

        if let Some((fence_ch, fence_len, _info)) = open_fence(line.text) {
            let mut end = i + 1;
            let mut closed = false;
            while end < lines.len() {
                if is_close_fence(lines[end].text, fence_ch, fence_len) {
                    closed = true;
                    break;
                }
                end += 1;
            }
            if !closed {
                diagnostics.push(ParseDiagnostic::warning(
                    SourceSpan::new(line.start, src.len()),
                    "unclosed fenced code block reaches end of document",
                ));
                break;
            }
            i = end;
        }

        i += 1;
    }

    diagnostics
}

fn looks_like_reference_definition(line: &str) -> bool {
    if leading_spaces(line) > 3 {
        return false;
    }
    let t = trim_start_space_tab(line);
    t.starts_with('[') && t.contains("]:")
}

fn collect_link_references_tracked<'a>(
    lines: Vec<&'a str>,
    tracker: Option<&mut destinations::DestinationTracker>,
) -> (Vec<&'a str>, ReferenceMap, bool, ConsumedReferenceLines) {
    let mut consumed = ConsumedReferenceLines::new();
    let mut refs = ReferenceMap::new();
    let kept_reference_candidate = collect_link_reference_metadata_into_tracked(
        &lines,
        Some(&mut consumed),
        &mut refs,
        tracker,
    );
    (lines, refs, kept_reference_candidate, consumed)
}

fn strip_consumed_references<'a>(lines: &[&'a str], consumed: &[Range<usize>]) -> Vec<&'a str> {
    let consumed_line_count: usize = consumed
        .iter()
        .map(|range| range.end.saturating_sub(range.start))
        .sum();
    let mut kept = Vec::with_capacity(lines.len().saturating_sub(consumed_line_count));
    let mut cursor = 0usize;

    for range in consumed {
        kept.extend_from_slice(&lines[cursor..range.start]);
        if consumed_reference_run_separates_tables(lines, range) {
            kept.push("");
        }
        cursor = range.end;
    }

    kept.extend_from_slice(&lines[cursor..]);
    kept
}

fn consumed_reference_run_separates_tables(lines: &[&str], range: &Range<usize>) -> bool {
    table_ends_at(lines, range.start) && table_body_row_starts_at(lines, range.end)
}

fn table_ends_at(lines: &[&str], end: usize) -> bool {
    if end < 2 || end > lines.len() {
        return false;
    }

    let mut first_row = end;
    while first_row > 0 {
        let previous = lines[first_row - 1];
        if !is_table_row_source(previous)
            && validated_table_delimiter_cell_count(previous).is_none()
        {
            break;
        }
        first_row -= 1;
    }

    let mut start = first_row;
    while start + 1 < end {
        if let Some(used) = table_extent(&lines[start..end]) {
            if start + used == end {
                return true;
            }
            start += used;
        } else {
            start += 1;
        }
    }
    false
}

fn table_body_row_starts_at(lines: &[&str], start: usize) -> bool {
    start < lines.len() && is_table_row_source(lines[start])
}

fn collect_link_reference_metadata(lines: &[&str]) -> (ConsumedReferenceLines, ReferenceMap) {
    let mut consumed = ConsumedReferenceLines::new();
    let mut refs = ReferenceMap::new();
    collect_link_reference_metadata_into(lines, Some(&mut consumed), &mut refs);
    (consumed, refs)
}

/// Detect a GFM footnote definition start: `[^id]: content`. Returns the id
/// and the inline content after the colon. Ids follow GFM (non-empty, no
/// whitespace or brackets) plus a byte cap; HTML/PDF delimiter characters are
/// allowed so a `[^a"b]: dest` line is not stolen as a link-reference definition.
fn scan_footnote_definition(line: &str) -> Option<(String, &str)> {
    let trimmed = trim_start_space_tab(line);
    let rest = trimmed.strip_prefix("[^")?;
    let close = rest.find(']')?;
    let id = &rest[..close];
    if !is_safe_footnote_id(id) {
        return None;
    }
    let after = rest[close + 1..].strip_prefix(':')?;
    // Trailing source spaces can introduce a hard break before a continuation.
    // Remove the definition's separator indentation, not its Markdown syntax.
    Some((id.to_string(), trim_start_space_tab(after)))
}

/// Cheap check: the line starts a footnote definition (`[^id]:`).
fn footnote_definition_marker(line: &str) -> bool {
    let trimmed = trim_start_space_tab(line);
    let Some(rest) = trimmed.strip_prefix("[^") else {
        return false;
    };
    let Some(close) = rest.find(']') else {
        return false;
    };
    let id = &rest[..close];
    is_safe_footnote_id(id) && rest[close + 1..].starts_with(':')
}

/// A note owns four-column-indented continuations, including intervening blank
/// lines. A blank followed by unindented content ends the note. Share this
/// boundary between AST parsing, reference collection, and source spans.
fn footnote_extent<T>(lines: &[T], text: impl Fn(&T) -> &str) -> usize {
    let mut used = 1;
    while used < lines.len() {
        let line = text(&lines[used]);
        if is_blank_line(line) {
            let mut next = used + 1;
            while next < lines.len() && is_blank_line(text(&lines[next])) {
                next += 1;
            }
            if next == lines.len() || leading_spaces(text(&lines[next])) < 4 {
                break;
            }
            used = next;
        } else if leading_spaces(line) >= 4 {
            used += 1;
        } else {
            break;
        }
    }
    used
}

fn footnote_body_lines<'a>(lines: &[&'a str], content: &'a str) -> (Vec<&'a str>, usize) {
    let used = footnote_extent(lines, |line| *line);
    let mut body = Vec::with_capacity(used);
    body.push(content);
    body.extend(lines[1..used].iter().map(|line| strip_n(line, 4)));
    (body, used)
}

/// Preserve the established whitespace of plain, single-paragraph notes. Rich
/// bodies keep their physical lines so fences, tables, lists, and paragraph
/// boundaries reach the block parser intact.
fn footnote_body_is_plain_paragraph(lines: &[&str]) -> bool {
    table_extent(lines).is_none()
        && lines[..lines.len().saturating_sub(1)].iter().all(|line| {
            // Joining physical lines with spaces would erase explicit hard
            // breaks. Escaped backslashes and entity-produced whitespace are
            // content; use the inline parser's source-delimiter rule here.
            !line.ends_with("  ")
                && line.bytes().rev().take_while(|&byte| byte == b'\\').count() % 2 == 0
        })
        && lines.iter().all(|line| {
            let trimmed = trim_start_space_tab(line);
            !trimmed.contains("]:")
                && !trimmed.starts_with("$$")
                && !is_definition_marker(trimmed)
                && list_marker(trimmed).is_none()
                && setext_underline(trimmed).is_none()
                && line_is_paragraph_text_with_scan(trimmed, scan_markdown_line(trimmed))
        })
}

/// GFM footnote ids: non-empty, no whitespace, no brackets.
///
/// ASCII controls are also rejected (they are never useful ids). Length is
/// capped so a hostile `[^aaaa…]:` cannot allocate unbounded id copies.
/// Characters that are special in HTML attributes or PDF strings (`" < >`
/// etc.) are allowed here; emitters must attribute-escape. Rejecting them in
/// the parser made `[^a"b]: dest` a link-reference definition and deleted the
/// note.
fn is_safe_footnote_id(id: &str) -> bool {
    const MAX_FOOTNOTE_ID_BYTES: usize = 128;
    if id.is_empty() || id.len() > MAX_FOOTNOTE_ID_BYTES {
        return false;
    }
    id.chars().all(is_safe_footnote_id_char)
}

fn is_safe_footnote_id_char(c: char) -> bool {
    if c.is_whitespace() || (c as u32) < 0x20 || c == '\u{7f}' {
        return false;
    }
    !matches!(c, '[' | ']')
}

fn collect_link_reference_metadata_into(
    lines: &[&str],
    consumed: Option<&mut ConsumedReferenceLines>,
    refs: &mut ReferenceMap,
) -> bool {
    collect_link_reference_metadata_into_tracked(lines, consumed, refs, None)
}

fn collect_link_reference_metadata_into_tracked(
    lines: &[&str],
    consumed: Option<&mut ConsumedReferenceLines>,
    refs: &mut ReferenceMap,
    tracker: Option<&mut destinations::DestinationTracker>,
) -> bool {
    collect_link_reference_metadata_at_depth(lines, consumed, refs, tracker, 0)
}

fn collect_link_reference_metadata_at_depth(
    lines: &[&str],
    mut consumed: Option<&mut ConsumedReferenceLines>,
    refs: &mut ReferenceMap,
    mut tracker: Option<&mut destinations::DestinationTracker>,
    depth: usize,
) -> bool {
    let mut i = 0usize;
    // Whether the current position is a lazy continuation of an open top-level
    // paragraph. A link reference definition can only be *defined* at a block
    // boundary; per CommonMark it cannot interrupt a paragraph. Tracking this
    // keeps a `[label]: dest`-looking continuation line from being extracted,
    // which used to silently delete the line and merge the surrounding text
    // (e.g. `foo\n[bar]: /url\nbaz` dropped the middle line).
    let mut in_paragraph = false;
    let mut kept_reference_candidate = false;

    while i < lines.len() {
        let line = lines[i];
        if is_blank_line(line) {
            in_paragraph = false;
            i += 1;
            continue;
        }

        // A display formula owns its entire literal body, including blank lines
        // and reference-shaped TeX. It cannot interrupt an open paragraph, and
        // its closing fence leaves the next line at a real block boundary.
        if !in_paragraph && let Some(start) = math_block_start(line) {
            i += math_block_extent(&lines[i..], start);
            continue;
        }

        // One-column tables may omit the header's pipes when the delimiter
        // supplies them. That ordinary-looking header needs the same lookahead
        // as the block parser before the plain-line fast path opens a paragraph.
        let possible_table = !in_paragraph
            && lines
                .get(i + 1)
                .is_some_and(|delimiter| line.contains('|') || delimiter.contains('|'));
        if !possible_table
            && let Some(is_paragraph_text) = reference_collector_plain_nonblank_line_fast_path(line)
        {
            // The nonblank fast path returns false for indented code at a
            // block boundary. Indentation cannot close an already open
            // paragraph: its hanging continuation still owns any following
            // reference-shaped text, just as in the real block parser.
            in_paragraph |= is_paragraph_text;
            i += 1;
            continue;
        }

        let scan = scan_markdown_line(line);
        // Fenced code block contents are literal text, never reference
        // definitions. Skip over the whole fence (matching the block parser's
        // own fence handling) so a `[label]: dest`-looking code line is not
        // extracted and silently deleted from the rendered code block.
        if scan.maybe_fence
            && let Some((fence_ch, fence_len, _info)) = open_fence(line)
        {
            let start = i;
            i += 1;
            while i < lines.len() && !is_close_fence(lines[i], fence_ch, fence_len) {
                i += 1;
            }
            i += 1; // step past the closing fence (or past end if unclosed)
            if depth > 0
                && let Some(tracker) = &mut tracker
            {
                tracker.container_fence(&lines[start..i.min(lines.len())]);
            }
            in_paragraph = false; // a code block is not paragraph text
            continue;
        }

        // A block quote is its own block. Consume the whole run — its `>` lines
        // plus any lazy paragraph continuations — exactly as the block parser
        // does, so a `[label]: dest` line that lazily
        // continues the quote's open paragraph is not mistaken for a top-level
        // boundary definition and stripped, which silently deleted the line and
        // phantom-defined the label. Recurse at this source position so earlier
        // quoted definitions keep precedence over later top-level/list ones.
        if scan.maybe_blockquote && blockquote_marker_start(line) {
            let checkpoint = tracker.as_ref().map_or(0, |tracker| tracker.checkpoint());
            let (inner, used) = split_blockquote_lines_tracked(&lines[i..], tracker.as_deref_mut());
            kept_reference_candidate |= lines[i..i + used].iter().any(|line| line.contains("]:"));
            i += used;
            if depth < MAX_BLOCK_NESTING_DEPTH {
                let inner_lines: Vec<&str> = inner.iter().map(|line| line.as_ref()).collect();
                collect_link_reference_metadata_at_depth(
                    &inner_lines,
                    None,
                    refs,
                    tracker.as_deref_mut(),
                    depth + 1,
                );
            }
            if let Some(tracker) = &mut tracker {
                tracker.restore(checkpoint);
            }
            in_paragraph = false;
            continue;
        }

        // HTML block contents are literal text, never reference definitions.
        // Skip the whole block (matching the block parser) so a `[label]: dest`-
        // looking line inside raw HTML is not extracted and resolved.
        let html_kind = scan.maybe_html.then(|| html_block_kind(line)).flatten();
        if let Some(end_cond) = html_kind
            && (!in_paragraph || end_cond.interrupts_paragraph())
        {
            i = html_block_end(lines, i, end_cond, |l| *l);
            in_paragraph = false;
            continue;
        }

        // GFM footnote definitions (`[^id]: content`) are NOT link reference
        // definitions: the footnote block parser consumes them later. Skip the
        // whole body here so it is not extracted as a `[label]: dest` link ref
        // (which would silently delete the footnote). Resolve its nested
        // definitions after removing the note indent, in source order.
        if !in_paragraph && let Some((_, content)) = scan_footnote_definition(line) {
            let (body, used) = footnote_body_lines(&lines[i..], content);
            kept_reference_candidate |= lines[i..i + used].iter().any(|line| line.contains("]:"));
            if depth < MAX_BLOCK_NESTING_DEPTH {
                collect_link_reference_metadata_at_depth(
                    &body,
                    None,
                    refs,
                    tracker.as_deref_mut(),
                    depth + 1,
                );
            }
            i += used;
            in_paragraph = false;
            continue;
        }

        // A GFM table is a distinct block, so a definition after it is at a block
        // boundary — skip the table's rows. A table cannot interrupt a paragraph,
        // so this only applies at a boundary (in_paragraph false); mid-paragraph
        // the rows are absorbed as ordinary continuation via line_is_paragraph_text.
        if possible_table && let Some(extent) = table_extent_with(&lines[i..], |line| *line) {
            i += extent.lines;
            // Padding exhaustion preserves the first unadmitted row as prose.
            // It must not gain reference-definition authority merely because
            // the preceding table reached its allocation bound.
            in_paragraph = extent.padding_exhausted;
            if extent.padding_exhausted {
                // This is the paragraph's first line, not its continuation.
                // Reprocessing a rejected `===` with in_paragraph=true would
                // wrongly close a setext heading and expose the next definition.
                kept_reference_candidate |= scan_markdown_line(lines[i]).maybe_reference;
                i += 1;
            }
            continue;
        }

        // Lists own their indented code and lazy paragraphs. Reuse their real
        // boundary before inspecting definitions, so a code example cannot
        // define a reference and a dedented definition after code is not lost.
        // Harvest item definitions now to retain first-definition source order.
        if scan.maybe_list_marker
            && let Some(marker) = list_marker(line)
            && (!in_paragraph || marker_interrupts_paragraph(&marker))
        {
            let checkpoint = tracker.as_ref().map_or(0, |tracker| tracker.checkpoint());
            let split = split_list_items_with_first_marker_tracked(
                &lines[i..],
                marker,
                tracker.as_deref_mut(),
            );
            let used = split.used.max(1);
            kept_reference_candidate |= lines[i..i + used].iter().any(|line| line.contains("]:"));
            if depth < MAX_BLOCK_NESTING_DEPTH {
                for (_, body) in &split.items {
                    let body_lines: Vec<&str> = body.iter().map(|line| line.as_ref()).collect();
                    collect_link_reference_metadata_at_depth(
                        &body_lines,
                        None,
                        refs,
                        tracker.as_deref_mut(),
                        depth + 1,
                    );
                }
            }
            if let Some(tracker) = &mut tracker {
                tracker.restore(checkpoint);
            }
            i += used;
            in_paragraph = false;
            continue;
        }

        // Extract a reference definition only at a block boundary, never as a
        // paragraph continuation.
        if scan.maybe_reference {
            if !in_paragraph
                && let Some((label, reference, used)) =
                    parse_multiline_reference_definition(&lines[i..])
            {
                if let Some(tracker) = &mut tracker {
                    tracker.reference(&lines[i..i + used], &label, &reference.dest);
                }
                refs.entry(label).or_insert(reference);
                if let Some(consumed) = consumed.as_mut() {
                    push_consumed_reference_range(consumed, i..i + used);
                }
                i += used;
                // Leading reference definitions do not themselves open a paragraph.
                in_paragraph = false;
                continue;
            }

            kept_reference_candidate = true;
        }

        // A setext underline (`===`/`---`) following paragraph text closes that
        // paragraph into a heading — the block parser does exactly this — so the
        // next line is at a block boundary. Without this, a `===` (which
        // `line_is_paragraph_text` treats as ordinary text) would keep the
        // paragraph "open" and a following reference definition would be wrongly
        // absorbed as a lazy continuation and dropped.
        if in_paragraph && scan.maybe_setext_underline && setext_underline(line).is_some() {
            in_paragraph = false;
            i += 1;
            continue;
        }

        // Any other non-blank line is paragraph text — so a following
        // `[label]: dest` line is a lazy continuation, not a definition — unless
        // it begins a different block, which closes/prevents a paragraph.
        in_paragraph = line_is_paragraph_text_with_scan(line, scan)
            || (in_paragraph && html_kind.is_some_and(|end| !end.interrupts_paragraph()));
        i += 1;
    }
    kept_reference_candidate
}

fn push_consumed_reference_range(consumed: &mut ConsumedReferenceLines, range: Range<usize>) {
    if range.start >= range.end {
        return;
    }
    if let Some(previous) = consumed.last_mut()
        && range.start <= previous.end
    {
        previous.end = previous.end.max(range.end);
        return;
    }
    consumed.push(range);
}

fn reference_collector_plain_nonblank_line_fast_path(line: &str) -> Option<bool> {
    let indent = leading_spaces(line);
    if indent >= 4 {
        return Some(false);
    }
    if reference_collector_needs_block_scan_after_indent(line, indent) {
        return None;
    }
    Some(true)
}

fn reference_collector_needs_block_scan_after_indent(line: &str, indent: usize) -> bool {
    let bytes = line.as_bytes();
    let tail = bytes.get(indent..).unwrap_or_default();
    let Some(&first) = tail.first() else {
        return false;
    };

    match first {
        b'[' | b'#' | b'>' | b'<' | b'`' | b'~' | b'=' | b'-' | b'*' | b'_' | b'+' => true,
        b'0'..=b'9' => reference_collector_ordered_marker_candidate(tail) || tail.contains(&b'|'),
        _ => tail.contains(&b'|'),
    }
}

fn reference_collector_ordered_marker_candidate(bytes: &[u8]) -> bool {
    let digits = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digits == 0 || digits > 9 {
        return false;
    }
    let Some((&marker, rest)) = bytes.get(digits..).and_then(|tail| tail.split_first()) else {
        return false;
    };
    matches!(marker, b'.' | b')') && rest.first().is_none_or(|byte| matches!(byte, b' ' | b'\t'))
}

/// If `lines` begins with a GFM pipe table (a row followed by a delimiter row of
/// matching column count), return how many lines it spans; otherwise `None`.
/// Mirrors `parse_table_profiled`'s extent + column-count validation exactly
/// (same `split_table_row`, same body-row loop) but without rendering cells, so
/// reference-definition collection can skip a table's rows — a table is a
/// distinct block, so a following definition is at a block boundary, not a
/// paragraph continuation.
fn table_extent(lines: &[&str]) -> Option<usize> {
    table_extent_with(lines, |line| *line).map(|extent| extent.lines)
}

struct TableExtent {
    lines: usize,
    padding_exhausted: bool,
}

fn table_extent_with<T>(lines: &[T], text: impl Fn(&T) -> &str) -> Option<TableExtent> {
    if lines.len() < 2 {
        return None;
    }
    let header = text(&lines[0]);
    let delimiter = text(&lines[1]);
    if !is_table_header_source(header, delimiter) {
        return None;
    }
    let delimiter_cols = validated_table_delimiter_cell_count(delimiter)?;
    let cols = split_table_row_cell_count(header);
    if cols == 0 || delimiter_cols != cols {
        return None;
    }
    let mut body = TableBodyState::new(cols);
    let mut i = 2;
    while i < lines.len() {
        let line = text(&lines[i]);
        if !is_table_row_source(line) {
            break;
        }
        if !body.admit_columns(split_table_row_cell_count(line)) {
            return Some(TableExtent {
                lines: i,
                padding_exhausted: true,
            });
        }
        i += 1;
    }
    Some(TableExtent {
        lines: i,
        padding_exhausted: false,
    })
}

// Only generated padding counts toward this limit; ordinary complete tables
// remain proportional to their source. At most 512K empty cell vectors (12 MiB
// on 64-bit targets) may be synthesized per table. The first row beyond this
// budget and its ordinary continuations stay intact as a paragraph.
const MAX_TABLE_PADDING_CELLS: usize = 512 * 1024;

#[derive(Clone, Copy)]
struct TableBodyState {
    columns: usize,
    padding_left: usize,
}

impl TableBodyState {
    const fn new(columns: usize) -> Self {
        Self {
            columns,
            padding_left: MAX_TABLE_PADDING_CELLS,
        }
    }

    fn admit_columns(&mut self, source_columns: usize) -> bool {
        let needed = self.columns.saturating_sub(source_columns);
        if needed > self.padding_left {
            return false;
        }
        self.padding_left -= needed;
        true
    }
}

/// A table needs an explicit pipe in its header or delimiter. Without either,
/// a one-column `Title\n---` remains a setext heading rather than a table.
fn is_table_header_source(header: &str, delimiter: &str) -> bool {
    (header.contains('|') || delimiter.contains('|')) && is_table_row_source(header)
}

/// Ordinary text becomes a table body row even without a pipe: GFM pads a short
/// row with empty cells. A table leaf must stay a table while containers decide
/// whether a dedented line is lazy. Block markers keep their priority: a list's
/// first line must not become a top-level table header, and a following fence
/// must not become a row whose
/// literal definitions gain scope. Share admission with reference collection
/// and container laziness. Preserve the established boundary for unpiped link
/// definition prefixes, including multiline labels/titles. Pipe-bearing
/// reference-looking source remains cell content, as before.
fn is_table_row_source(line: &str) -> bool {
    if is_blank_line(line) {
        return false;
    }
    if !line.contains('|')
        && trim_start_space_tab(line).starts_with('[')
        && ReferenceDefinitionScan::new().line(line).is_some()
    {
        return false;
    }
    let scan = BlockStartScan::new(line, trim_space_tab(line));
    !scanned_thematic_break(scan)
        && scanned_atx_heading(scan).is_none()
        && scanned_open_fence(scan).is_none()
        && !scanned_indented_code_start(scan)
        && !scanned_blockquote_start(scan)
        && scanned_html_block_kind(scan).is_none()
        && scanned_list_marker(scan).is_none()
        && !footnote_definition_marker(line)
        && math_block_start(line).is_none()
}

fn line_is_paragraph_text_with_scan(line: &str, scan: ParserLineScan) -> bool {
    if is_blank_line(line) {
        return false;
    }

    if line_is_plain_paragraph_fast_path(line, scan) {
        return true;
    }

    (!scan.maybe_thematic_break || !is_thematic_break(line))
        && (!scan.maybe_heading_marker || atx_heading(line).is_none())
        && (!scan.maybe_fence || open_fence(line).is_none())
        && !indented_code_start(line)
        && (!scan.maybe_blockquote || !blockquote_marker_start(line))
        && (!scan.maybe_html || !html_block_start(line))
        && (!scan.maybe_list_marker || !list_marker_interrupts_paragraph(line))
}

fn line_is_plain_paragraph_fast_path(line: &str, scan: ParserLineScan) -> bool {
    if line
        .as_bytes()
        .first()
        .is_some_and(|byte| is_space_or_tab_byte(*byte))
    {
        return false;
    }

    !scan.maybe_thematic_break
        && !scan.maybe_heading_marker
        && !scan.maybe_fence
        && !scan.maybe_blockquote
        && !scan.maybe_html
        && !scan.maybe_list_marker
}
/// Defensive recursion bound for block nesting (blockquotes, lists). Real
/// documents nest only a handful of levels; this cap is far beyond any legitimate
/// use yet low enough that even a debug build (with large stack frames) cannot
/// overflow on adversarial input such as `">".repeat(100_000)`.
const MAX_BLOCK_NESTING_DEPTH: usize = 128;

/// Recurse into nested block content (a blockquote body or a list item body) with
/// a depth guard. Past [`MAX_BLOCK_NESTING_DEPTH`] levels, stop nesting and emit
/// the remaining content as one flat paragraph instead of recursing — this keeps
/// the text while making a stack-overflow DoS on deeply-nested untrusted input
/// impossible. Inline parsing is iterative (bounded), so the fallback is safe.
fn parse_blocks_bounded(
    lines: &[&str],
    refs: &ReferenceMap,
    profiler: &mut ParseProfiler,
) -> Vec<Block> {
    let consumed = if lines.iter().any(|line| line.contains("]:")) {
        collect_link_reference_metadata(lines).0
    } else {
        Vec::new()
    };
    if profiler.block_depth >= MAX_BLOCK_NESTING_DEPTH {
        let kept = strip_consumed_references(lines, &consumed);
        let text = kept.join("\n");
        let trimmed = trim_unicode_ws(&text);
        if trimmed.is_empty() {
            return Vec::new();
        }
        return vec![Block::Paragraph(parse_inlines_with_refs_profiled(
            trimmed, refs, profiler,
        ))];
    }
    profiler.block_depth += 1;
    let blocks = parse_blocks_with_reference_boundaries(lines, &consumed, refs, profiler);
    profiler.block_depth -= 1;
    blocks
}

#[derive(Clone, Copy)]
struct BlockStartScan<'a> {
    line: &'a str,
    trimmed: &'a str,
    indent: usize,
}

impl<'a> BlockStartScan<'a> {
    fn new(line: &'a str, trimmed: &'a str) -> Self {
        Self {
            line,
            trimmed,
            indent: leading_spaces(line),
        }
    }

    fn after_indent(self) -> Option<&'a str> {
        (self.indent <= 3).then(|| &self.line[self.indent..])
    }

    fn after_indent_first(self) -> Option<u8> {
        self.after_indent()
            .and_then(|tail| tail.as_bytes().first().copied())
    }

    fn first_trimmed(self) -> Option<u8> {
        self.trimmed.as_bytes().first().copied()
    }
}

fn scanned_thematic_break(scan: BlockStartScan<'_>) -> bool {
    scan.indent <= 3
        && matches!(scan.first_trimmed(), Some(b'-' | b'*' | b'_'))
        && thematic_break_trimmed(scan.trimmed)
}

fn scanned_atx_heading(scan: BlockStartScan<'_>) -> Option<(u8, &str)> {
    let tail = scan.after_indent()?;
    (tail.as_bytes().first() == Some(&b'#'))
        .then(|| atx_heading_after_indent(tail))
        .flatten()
}

fn scanned_open_fence(scan: BlockStartScan<'_>) -> Option<(char, usize, &str)> {
    let tail = scan.after_indent()?;
    matches!(tail.as_bytes().first(), Some(b'`' | b'~'))
        .then(|| open_fence_after_indent(tail))
        .flatten()
}

fn scanned_indented_code_start(scan: BlockStartScan<'_>) -> bool {
    scan.indent >= 4
}

fn scanned_blockquote_start(scan: BlockStartScan<'_>) -> bool {
    scan.after_indent_first() == Some(b'>')
}

fn scanned_html_block_kind(scan: BlockStartScan<'_>) -> Option<HtmlBlockEnd> {
    scan.after_indent()
        .filter(|tail| tail.as_bytes().first() == Some(&b'<'))
        .and_then(html_block_kind_from_block_start)
}

fn scanned_html_paragraph_interrupt(scan: BlockStartScan<'_>) -> bool {
    scanned_html_block_kind(scan).is_some_and(HtmlBlockEnd::interrupts_paragraph)
}

fn scanned_list_marker(scan: BlockStartScan<'_>) -> Option<Marker<'_>> {
    matches!(
        scan.after_indent_first(),
        Some(b'-' | b'*' | b'+' | b'0'..=b'9')
    )
    .then(|| list_marker(scan.line))
    .flatten()
}

fn scanned_list_marker_interrupts_paragraph(scan: BlockStartScan<'_>) -> bool {
    scanned_list_marker(scan)
        .as_ref()
        .is_some_and(marker_interrupts_paragraph)
}

fn scanned_setext_underline(scan: BlockStartScan<'_>) -> Option<u8> {
    (scan.indent <= 3 && matches!(scan.first_trimmed(), Some(b'=' | b'-')))
        .then(|| setext_underline_trimmed(scan.trimmed))
        .flatten()
}

fn scanned_paragraph_interrupt(scan: BlockStartScan<'_>) -> bool {
    // CommonMark: an indented code block cannot interrupt a paragraph (it is
    // hanging indent / lazy continuation). The block-start path still opens
    // indented code at a real boundary.
    scanned_thematic_break(scan)
        || scanned_atx_heading(scan).is_some()
        || scanned_open_fence(scan).is_some()
        || scanned_blockquote_start(scan)
        || scanned_html_paragraph_interrupt(scan)
        || scanned_list_marker_interrupts_paragraph(scan)
}

/// Definitions are invisible content, but still terminate their source blocks.
/// Parsing the original slices on either side prevents distinct lists, code
/// blocks or tables from being joined when their separating definition vanishes.
fn parse_blocks_with_reference_boundaries(
    lines: &[&str],
    consumed: &[Range<usize>],
    refs: &ReferenceMap,
    profiler: &mut ParseProfiler,
) -> Vec<Block> {
    if consumed.is_empty() {
        return parse_blocks_with_refs_profiled(lines, refs, profiler, 0);
    }
    let mut blocks = Vec::new();
    let mut start = 0;
    for range in consumed {
        blocks.extend(parse_blocks_with_refs_profiled(
            &lines[start..range.start],
            refs,
            profiler,
            start,
        ));
        start = range.end;
    }
    blocks.extend(parse_blocks_with_refs_profiled(
        &lines[start..],
        refs,
        profiler,
        start,
    ));
    blocks
}

fn parse_blocks_with_refs_profiled(
    lines: &[&str],
    refs: &ReferenceMap,
    profiler: &mut ParseProfiler,
    source_line_offset: usize,
) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut i = 0;
    let mut table_padding_fallback = false;
    // A nonblank iteration emits one block. Record its consumed lines at the
    // next iteration (or EOF), so every existing and future block branch uses
    // the same boundaries as its semantic parse. Nested container calls must
    // not consume entries in the top-level source map.
    let track_spans = profiler.block_depth == 0 && profiler.block_spans.is_some();
    let mut block_start = None;
    'blocks: while i < lines.len() {
        if let Some(start) = block_start.take()
            && let Some(spans) = &mut profiler.block_spans
        {
            spans.record(start + source_line_offset, i + source_line_offset);
        }
        let line = lines[i];
        let force_paragraph = std::mem::take(&mut table_padding_fallback);
        let trimmed = trim_space_tab(line);
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        if track_spans {
            block_start = Some(i);
        }
        let scan = BlockStartScan::new(line, trimmed);
        if scanned_thematic_break(scan) {
            blocks.push(Block::ThematicBreak);
            i += 1;
            continue;
        }
        if let Some((level, text)) = scanned_atx_heading(scan) {
            let started = profiler.checkpoint();
            let inlines = parse_inlines_with_refs_profiled(text, refs, profiler);
            profiler.record_since(
                "heading_block",
                1,
                line.len(),
                1 + inlines.len(),
                "parse one ATX heading block and its inline content",
                started,
            );
            blocks.push(Block::Heading { level, inlines });
            i += 1;
            continue;
        }
        if let Some((fence_ch, fence_len, info)) = scanned_open_fence(scan) {
            let started = profiler.checkpoint();
            let lang = {
                let decoded = decode_info_string(trim_unicode_ws(info));
                decoded
                    .split_whitespace()
                    .next()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            // CommonMark: up to N columns of leading indentation are removed from
            // each content line, where N is the opening fence's own indentation.
            let fence_indent = leading_spaces(line);
            // Presize the body: sum the raw byte lengths of the content lines
            // (plus one newline each) up to the closing fence. Stripping the
            // fence indent only shrinks lines, so this over-estimates at
            // most; capacity is invisible to the collected bytes.
            let mut body_bytes = 0usize;
            {
                let mut scan = i + 1;
                while scan < lines.len() && !is_close_fence(lines[scan], fence_ch, fence_len) {
                    body_bytes += lines[scan].len() + 1;
                    scan += 1;
                }
            }
            let mut code = String::with_capacity(body_bytes);
            let mut code_parts = Vec::new();
            i += 1;
            while i < lines.len() {
                if is_close_fence(lines[i], fence_ch, fence_len) {
                    i += 1;
                    break;
                }
                let part = strip_fence_indent(lines[i], fence_indent);
                code_parts.push((code.len(), part));
                code.push_str(part);
                code.push('\n');
                i += 1;
            }
            profiler.record_since(
                "fenced_code_block",
                1,
                code.len(),
                usize::from(lang.is_some()) + 1,
                "parse one fenced code block body and language info",
                started,
            );
            register_parts(&code, &code_parts, profiler);
            if lang.as_deref() == Some("math") {
                blocks.push(Block::MathBlock(provenance::copy(
                    code.trim_end_matches('\n'),
                )));
            } else {
                blocks.push(Block::CodeBlock { lang, code });
            }
            continue;
        }
        if let Some(start) = math_block_start(line) {
            let started = profiler.checkpoint();
            let used = math_block_extent(&lines[i..], start);
            let multiline = matches!(start, MathBlockStart::Multiline(_));
            let mut math_parts = Vec::new();
            let mut code = match start {
                MathBlockStart::SingleLine(content) => {
                    let text = content.to_owned();
                    provenance::register(&text, provenance::text_origins(content, profiler));
                    text
                }
                MathBlockStart::Multiline(initial) => {
                    let mut code = String::new();
                    if !initial.is_empty() {
                        math_parts.push((code.len(), initial));
                        code.push_str(initial);
                        code.push('\n');
                    }
                    for line in &lines[i + 1..i + used] {
                        if let Some(inner) = math_block_close(line) {
                            if !inner.is_empty() {
                                math_parts.push((code.len(), inner));
                                code.push_str(inner);
                                code.push('\n');
                            }
                        } else {
                            math_parts.push((code.len(), *line));
                            code.push_str(line);
                            code.push('\n');
                        }
                    }
                    code
                }
            };
            profiler.record_since(
                "math_block",
                used,
                code.len(),
                1,
                if multiline {
                    "parse one multi-line math block"
                } else {
                    "parse one single-line math block"
                },
                started,
            );
            if multiline {
                register_parts(&code, &math_parts, profiler);
                code.truncate(code.trim_end_matches('\n').len());
            }
            blocks.push(Block::MathBlock(code));
            i += used;
            continue;
        }
        if scanned_indented_code_start(scan) {
            let started = profiler.checkpoint();
            let (code, used) = parse_indented_code(&lines[i..], profiler);
            profiler.record_since(
                "indented_code_block",
                used,
                code.len(),
                1,
                "parse one indented code block",
                started,
            );
            blocks.push(Block::CodeBlock { lang: None, code });
            i += used;
            continue;
        }
        // GFM footnote definition with a bounded sequence of semantic blocks.
        // Keep blank-separated continuations inside the note and remove only
        // its four-column container indent before parsing the nested blocks.
        if let Some((id, content)) = scan_footnote_definition(line) {
            let started = profiler.checkpoint();
            let checkpoint = profiler.destination_checkpoint();
            let (body, used) = footnote_body_lines(&lines[i..], content);
            let byte_len = body.iter().map(|line| line.len()).sum();
            let inner_blocks = if footnote_body_is_plain_paragraph(&body) {
                let mut text = String::new();
                let mut parts = profiler.destinations.is_some().then(Vec::new);
                for (index, line) in body.iter().enumerate() {
                    // The plain-note shortcut historically trimmed the first
                    // line after its marker. Keep those bytes stable when no
                    // explicit hard break requires the physical-line parser.
                    let line = if index == 0 {
                        trim_end_space_tab(line)
                    } else {
                        *line
                    };
                    if index > 0 {
                        text.push(' ');
                    }
                    if let Some(parts) = &mut parts {
                        parts.push((text.len(), line));
                    }
                    text.push_str(line);
                }
                if let Some(parts) = parts {
                    profiler.destination_parts(&text, &parts);
                }
                vec![Block::Paragraph(parse_inlines_with_refs_profiled(
                    &text, refs, profiler,
                ))]
            } else {
                parse_blocks_bounded(&body, refs, profiler)
            };
            profiler.record_since(
                "footnote_definition",
                used,
                byte_len,
                inner_blocks.len(),
                "parse one GFM footnote definition and its block content",
                started,
            );
            blocks.push(Block::FootnoteDefinition {
                id,
                blocks: inner_blocks,
            });
            profiler.destination_restore(checkpoint);
            i += used;
            continue;
        }
        if scanned_blockquote_start(scan) {
            let started = profiler.checkpoint();
            let checkpoint = profiler.destination_checkpoint();
            let (inner, used) =
                split_blockquote_lines_tracked(&lines[i..], profiler.destinations.as_mut());
            i += used;
            let inner_line_count = inner.len();
            let inner_bytes = if profiler.enabled {
                inner.iter().map(|line| line.len()).sum()
            } else {
                0
            };
            let inner_str_refs: Vec<&str> = inner.iter().map(|s| s.as_ref()).collect();
            let inner_blocks = parse_blocks_bounded(&inner_str_refs, refs, profiler);
            profiler.record_since(
                "blockquote_block",
                inner_line_count,
                inner_bytes,
                inner_line_count + inner_blocks.len(),
                "parse one blockquote and recursively parse its inner blocks",
                started,
            );
            blocks.push(Block::BlockQuote(inner_blocks));
            profiler.destination_restore(checkpoint);
            continue;
        }
        if let Some(end_cond) = scanned_html_block_kind(scan) {
            let started = profiler.checkpoint();
            let start = i;
            i = html_block_end(lines, i, end_cond, |l| *l);
            let html = lines[start..i].join("\n");
            let mut offset = 0;
            let parts: Vec<_> = lines[start..i]
                .iter()
                .map(|line| {
                    let part = (offset, *line);
                    offset += line.len() + 1;
                    part
                })
                .collect();
            register_parts(&html, &parts, profiler);
            if let Some(tracker) = &mut profiler.destinations {
                tracker.block_html(&html, &lines[start..i]);
            }
            profiler.record_since(
                "html_block",
                i - start,
                html.len(),
                1,
                "parse one raw HTML block",
                started,
            );
            blocks.push(Block::HtmlBlock(html));
            continue;
        }
        if !force_paragraph
            && i + 1 < lines.len()
            && is_table_header_source(line, lines[i + 1])
            && let Some(align) = validated_table_delimiter_alignments(lines[i + 1])
        {
            let started = profiler.checkpoint();
            if let Some((table, extent)) = parse_table_profiled(&lines[i..], refs, profiler, align)
            {
                let used = extent.lines;
                profiler.record_since(
                    "table_block",
                    used,
                    if profiler.enabled {
                        lines[i..i + used].iter().map(|line| line.len()).sum()
                    } else {
                        0
                    },
                    if profiler.enabled {
                        1 + table.head.len() + table.rows.iter().map(Vec::len).sum::<usize>()
                    } else {
                        0
                    },
                    "parse one pipe table block including aligned cells",
                    started,
                );
                blocks.push(Block::Table(table));
                i += used;
                table_padding_fallback = extent.padding_exhausted;
                continue;
            }
        }
        if let Some(marker) = scanned_list_marker(scan) {
            let started = profiler.checkpoint();
            let (list, used) =
                parse_list_profiled_with_first_marker(&lines[i..], refs, profiler, marker);
            profiler.record_since(
                "list_block",
                used,
                if profiler.enabled {
                    lines[i..i + used].iter().map(|line| line.len()).sum()
                } else {
                    0
                },
                1 + list.items.len(),
                "parse one ordered/unordered/task list block",
                started,
            );
            blocks.push(Block::List(list));
            i += used;
            continue;
        }
        if !force_paragraph
            && let Some((dl, used)) = parse_definition_list_profiled(&lines[i..], refs, profiler)
        {
            let started = profiler.checkpoint();
            profiler.record_since(
                "definition_list_block",
                used,
                if profiler.enabled {
                    lines[i..i + used].iter().map(|line| line.len()).sum()
                } else {
                    0
                },
                1,
                "parse one definition list block",
                started,
            );
            blocks.push(dl);
            i += used;
            continue;
        }
        // Paragraph: collect until a blank line or the start of another block.
        let start = i;
        while i < lines.len() {
            let line = lines[i];
            if no_indent_paragraph_loop_fast_path(line, i > start) {
                i += 1;
                continue;
            }
            let trimmed = trim_space_tab(line);
            if trimmed.is_empty() {
                break;
            }
            let scan = BlockStartScan::new(line, trimmed);
            if i > start
                && let Some(level) = scanned_setext_underline(scan)
            {
                let started = profiler.checkpoint();
                let (inlines, text_len, handoff_allocations) =
                    parse_lines_as_inlines(&lines[start..i], refs, profiler);
                profiler.record_since(
                    "setext_heading_block",
                    i - start + 1,
                    text_len,
                    handoff_allocations + 1 + inlines.len(),
                    "parse one setext heading and its inline content",
                    started,
                );
                blocks.push(Block::Heading { level, inlines });
                i += 1;
                continue 'blocks;
            }
            if scanned_paragraph_interrupt(scan) {
                break;
            }
            i += 1;
        }
        let started = profiler.checkpoint();
        let (inlines, text_len, handoff_allocations) =
            parse_lines_as_inlines(&lines[start..i], refs, profiler);
        profiler.record_since(
            "paragraph_block",
            i - start,
            text_len,
            handoff_allocations + 1 + inlines.len(),
            "parse one paragraph block and its inline content",
            started,
        );
        blocks.push(Block::Paragraph(inlines));
    }
    if let Some(start) = block_start
        && let Some(spans) = &mut profiler.block_spans
    {
        spans.record(start + source_line_offset, i + source_line_offset);
    }
    blocks
}

fn no_indent_paragraph_loop_fast_path(line: &str, open_paragraph: bool) -> bool {
    let Some((&first, bytes)) = line.as_bytes().split_first() else {
        return false;
    };

    match first {
        b' ' | b'\t' | b'#' | b'>' | b'<' | b'`' | b'~' | b'=' | b'-' | b'*' | b'_' | b'+' => false,
        b'0'..=b'9' => open_paragraph && !no_indent_ordered_list_interrupt_candidate(first, bytes),
        _ => true,
    }
}

fn no_indent_ordered_list_interrupt_candidate(first_digit: u8, rest: &[u8]) -> bool {
    let mut digits = 1usize;
    let mut start = u64::from(first_digit - b'0');
    while let Some(&byte) = rest.get(digits - 1)
        && byte.is_ascii_digit()
    {
        digits += 1;
        if digits > 9 {
            return false;
        }
        start = start * 10 + u64::from(byte - b'0');
    }
    if start != 1 {
        return false;
    }
    let Some((&marker, after_marker)) = rest.get(digits - 1..).and_then(|tail| tail.split_first())
    else {
        return false;
    };
    matches!(marker, b'.' | b')')
        && after_marker
            .first()
            .is_none_or(|byte| matches!(byte, b' ' | b'\t'))
}

fn parse_lines_as_inlines(
    lines: &[&str],
    refs: &ReferenceMap,
    profiler: &mut ParseProfiler,
) -> (Vec<Inline>, usize, usize) {
    match lines {
        [] => (Vec::new(), 0, 0),
        [line] => {
            let trimmed = line
                .trim_start_matches([' ', '\t'])
                .trim_end_matches([' ', '\t']);
            (
                parse_inlines_with_refs_profiled(trimmed, refs, profiler),
                line.len(),
                0,
            )
        }
        _ => {
            let started = profiler.checkpoint();
            let scan = scan_multiline_inline_lines(lines);
            if !scan.needs_full_parse && profiler.destinations.is_none() {
                let (inlines, char_count) =
                    plain_multiline_inline_fast_path(lines, profiler.enabled);
                profiler.record_since(
                    "inline_parse",
                    char_count,
                    scan.byte_len,
                    inlines.len(),
                    INLINE_PARSE_NOTES,
                    started,
                );
                return (inlines, scan.byte_len, 0);
            }
            let chars = collect_inline_chars_from_lines(lines, scan.byte_len);
            if let Some(tracker) = &mut profiler.destinations {
                tracker.push_chars_lines(&chars, lines);
            }
            let parsed = parse_inlines_chars_with_refs_profiled(
                &chars,
                scan.byte_len,
                refs,
                InlineContext::Prose,
                profiler,
                started,
            );
            if let Some(tracker) = &mut profiler.destinations {
                tracker.pop_chars();
            }
            (parsed, scan.byte_len, 0)
        }
    }
}

struct MultilineInlineScan {
    byte_len: usize,
    needs_full_parse: bool,
}

fn scan_multiline_inline_lines(lines: &[&str]) -> MultilineInlineScan {
    let mut byte_len = lines.len().saturating_sub(1);
    let mut needs_full_parse = false;
    for line in lines {
        byte_len += line.len();
        if !needs_full_parse && inline_text_needs_full_parse(line) {
            needs_full_parse = true;
        }
    }
    MultilineInlineScan {
        byte_len,
        needs_full_parse,
    }
}

fn plain_multiline_inline_fast_path(lines: &[&str], count_chars: bool) -> (Vec<Inline>, usize) {
    let mut out = Vec::with_capacity(lines.len().saturating_mul(2).saturating_sub(1));
    let mut char_count = if count_chars {
        lines.len().saturating_sub(1)
    } else {
        0
    };
    for (idx, line) in lines.iter().enumerate() {
        let trimmed_start = line.trim_start_matches([' ', '\t']);
        if count_chars {
            char_count += trimmed_start.chars().count();
        }
        let is_last = idx + 1 == lines.len();
        let hard = line.ends_with("  ") || line.ends_with('\\');
        let text = if is_last {
            trimmed_start.trim_end_matches([' ', '\t'])
        } else {
            trimmed_start.trim_end_matches(' ')
        };
        push_inline_text(&mut out, text);
        if !is_last {
            out.push(if hard {
                Inline::HardBreak
            } else {
                Inline::SoftBreak
            });
        }
    }
    (out, char_count)
}

fn collect_inline_chars_from_lines(lines: &[&str], byte_len: usize) -> Vec<char> {
    let mut chars = Vec::with_capacity(byte_len);
    for (idx, line) in lines.iter().enumerate() {
        if idx > 0 {
            chars.push('\n');
        }
        let trimmed_start = line.trim_start_matches([' ', '\t']);
        let trimmed = if idx + 1 == lines.len() {
            trimmed_start.trim_end_matches([' ', '\t'])
        } else {
            trimmed_start
        };
        chars.extend(trimmed.chars());
    }
    chars
}

fn collect_inline_chars_from_text(text: &str) -> Vec<char> {
    let mut chars = Vec::with_capacity(text.len());
    let mut source = text.chars().peekable();
    while let Some(ch) = source.next() {
        if ch == '\r' {
            if source.peek() == Some(&'\n') {
                let _ = source.next();
            }
            chars.push('\n');
        } else {
            chars.push(ch);
        }
    }
    chars
}

fn skip_spaces_or_newline(chars: &[char], i: &mut usize) {
    while *i < chars.len() && (chars[*i] == ' ' || chars[*i] == '\t' || chars[*i] == '\n') {
        *i += 1;
    }
}

#[derive(Clone, Copy)]
enum ReferencePhase {
    Label,
    Colon,
    DestinationStart,
    AngleDestination,
    BareDestination,
    AfterDestination { separated: bool },
    NextLineTitle { indented_code: bool },
    Title { close: char },
    AfterTitle,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReferenceProgress {
    Pending,
    Destination,
    Title,
}

/// Locate a definition without repeatedly joining and parsing its prefixes.
/// The same constant-size state follows reference candidates while containers
/// are collected. Only a completed definition is allocated and decoded by the
/// ordinary label/destination/title grammar.
///
/// Labels stop after 999 source characters, destinations cannot cross a line,
/// and titles stop at their first unescaped closing delimiter (or an invalid
/// open parenthesis). A later candidate's title opener therefore also ends an
/// earlier scan using that delimiter; failed candidates cannot each rescan an
/// arbitrarily long common suffix. Container observation visits each line once.
struct ReferenceDefinitionScan {
    phase: ReferencePhase,
    lines: usize,
    label_chars: usize,
    label_has_text: bool,
    escaped: bool,
    paren_depth: usize,
    title_saw_newline: bool,
}

impl ReferenceDefinitionScan {
    fn new() -> Self {
        Self {
            phase: ReferencePhase::Label,
            lines: 0,
            label_chars: 0,
            label_has_text: false,
            escaped: false,
            paren_depth: 0,
            title_saw_newline: false,
        }
    }

    fn line(&mut self, line: &str) -> Option<ReferenceProgress> {
        if is_blank_line(line) {
            return None;
        }
        let line = if self.lines == 0 {
            if leading_spaces(line) > 3 {
                return None;
            }
            trim_start_space_tab(line).strip_prefix('[')?
        } else {
            if matches!(
                self.phase,
                ReferencePhase::NextLineTitle {
                    indented_code: true
                }
            ) && leading_spaces(line) >= 4
            {
                return None;
            }
            line
        };
        self.lines += 1;
        for ch in line.chars() {
            match self.phase {
                ReferencePhase::Label => {
                    if !self.escaped && ch == ']' {
                        if !self.label_has_text {
                            return None;
                        }
                        self.phase = ReferencePhase::Colon;
                        continue;
                    }
                    if !self.escaped && ch == '[' {
                        return None;
                    }
                    self.label_chars += 1;
                    if self.label_chars > MAX_REFERENCE_LABEL_LEN {
                        return None;
                    }
                    self.label_has_text |= !ch.is_whitespace();
                    self.escaped = !self.escaped && ch == '\\';
                }
                ReferencePhase::Colon => {
                    if ch != ':' {
                        return None;
                    }
                    self.phase = ReferencePhase::DestinationStart;
                }
                ReferencePhase::DestinationStart => match ch {
                    ' ' | '\t' => {}
                    '<' => self.phase = ReferencePhase::AngleDestination,
                    ')' => return None,
                    ch if ch.is_ascii_control() => return None,
                    _ => {
                        self.phase = ReferencePhase::BareDestination;
                        self.paren_depth = usize::from(ch == '(');
                        self.escaped = ch == '\\';
                    }
                },
                ReferencePhase::AngleDestination | ReferencePhase::BareDestination => {
                    if self.escaped {
                        self.escaped = false;
                        if is_ascii_punct(ch) {
                            continue;
                        }
                    }
                    match (self.phase, ch) {
                        (_, '\\') => self.escaped = true,
                        (_, '<') => return None,
                        (ReferencePhase::AngleDestination, '>') => {
                            self.phase = ReferencePhase::AfterDestination { separated: false };
                        }
                        (ReferencePhase::AngleDestination, _) => {}
                        (_, ' ' | '\t' | '\r') if self.paren_depth == 0 => {
                            self.phase = ReferencePhase::AfterDestination { separated: true };
                        }
                        (_, '(') => self.paren_depth += 1,
                        (_, ')') if self.paren_depth > 0 => self.paren_depth -= 1,
                        (_, ')') => return None,
                        (_, ch) if ch.is_ascii_control() || ch == ' ' => return None,
                        _ => {}
                    }
                }
                ReferencePhase::AfterDestination { .. } | ReferencePhase::NextLineTitle { .. } => {
                    if matches!(ch, ' ' | '\t') {
                        if matches!(self.phase, ReferencePhase::AfterDestination { .. }) {
                            self.phase = ReferencePhase::AfterDestination { separated: true };
                        }
                        continue;
                    }
                    if matches!(
                        self.phase,
                        ReferencePhase::AfterDestination { separated: false }
                    ) {
                        return None;
                    }
                    let close = match ch {
                        '"' | '\'' => ch,
                        '(' => ')',
                        _ => return None,
                    };
                    self.phase = ReferencePhase::Title { close };
                    self.escaped = false;
                    self.title_saw_newline = false;
                }
                ReferencePhase::Title { close } => {
                    if self.escaped {
                        self.escaped = false;
                        if is_ascii_punct(ch) {
                            self.title_saw_newline = false;
                            continue;
                        }
                    }
                    if ch == close {
                        self.phase = ReferencePhase::AfterTitle;
                    } else if ch == '(' && close == ')' {
                        return None;
                    } else {
                        if !ch.is_whitespace() {
                            self.title_saw_newline = false;
                        }
                        self.escaped = ch == '\\';
                    }
                }
                ReferencePhase::AfterTitle => {
                    if !matches!(ch, ' ' | '\t') {
                        return None;
                    }
                }
            }
        }
        self.escaped = false;
        match self.phase {
            ReferencePhase::Label => {
                // A physical line ending is also one source label character.
                self.label_chars += 1;
                (self.label_chars <= MAX_REFERENCE_LABEL_LEN).then_some(ReferenceProgress::Pending)
            }
            ReferencePhase::DestinationStart => Some(ReferenceProgress::Pending),
            ReferencePhase::BareDestination if self.paren_depth != 0 => None,
            ReferencePhase::BareDestination | ReferencePhase::AfterDestination { .. } => {
                self.phase = ReferencePhase::NextLineTitle {
                    indented_code: self.lines == 1,
                };
                Some(ReferenceProgress::Destination)
            }
            ReferencePhase::Title { .. } if !self.title_saw_newline => {
                self.title_saw_newline = true;
                Some(ReferenceProgress::Pending)
            }
            ReferencePhase::AfterTitle => Some(ReferenceProgress::Title),
            _ => None,
        }
    }
}

fn parse_multiline_reference_definition(lines: &[&str]) -> Option<(String, LinkReference, usize)> {
    let first = *lines.first()?;
    if leading_spaces(first) > 3 || !trim_start_space_tab(first).starts_with('[') {
        return None;
    }

    // Keep the common one-line definition and next-line title fast paths.
    if let Some((label, reference)) = parse_reference_definition(first) {
        if reference.title.is_some() {
            return Some((label, reference, 1));
        }
        let Some(next) = lines.get(1).filter(|line| leading_spaces(line) < 4) else {
            return Some((label, reference, 1));
        };
        if let Some(title) = parse_reference_title_line(next) {
            return Some((
                label,
                LinkReference {
                    dest: reference.dest,
                    title: Some(title),
                },
                2,
            ));
        }
        if !trim_start_space_tab(next).starts_with(['"', '\'', '(']) {
            return Some((label, reference, 1));
        }
    }

    let mut scan = ReferenceDefinitionScan::new();
    let mut accepted = None;
    for (index, line) in lines.iter().enumerate() {
        match scan.line(line) {
            Some(ReferenceProgress::Pending) => {}
            Some(ReferenceProgress::Destination) => accepted = Some(index + 1),
            Some(ReferenceProgress::Title) => {
                accepted = Some(index + 1);
                break;
            }
            None => break,
        }
    }
    let used = accepted?;
    let joined = lines[..used].join("\n");
    let (label, reference) = parse_reference_definition(&joined)?;
    Some((label, reference, used))
}

fn parse_reference_definition(line: &str) -> Option<(String, LinkReference)> {
    if let Some(reference) = parse_simple_ascii_reference_definition(line) {
        return Some(reference);
    }

    if leading_spaces(line) > 3 {
        return None;
    }
    let t = trim_start_space_tab(line);
    let chars: Vec<char> = t.chars().collect();
    if chars.first() != Some(&'[') {
        return None;
    }
    let close = find_closing_bracket(&chars, 0)?;
    if chars.get(close + 1) != Some(&':') {
        return None;
    }
    let label = normalize_reference_label_chars(&chars[1..close])?;
    let mut i = close + 2;
    skip_spaces_or_newline(&chars, &mut i);
    if i >= chars.len() {
        return None;
    }

    let dest = parse_link_destination(&chars, &mut i)?;

    let spaces_start = i;
    skip_spaces_or_newline(&chars, &mut i);
    let title = if i >= chars.len() {
        None
    } else {
        if i == spaces_start {
            return None;
        }
        let title = parse_link_title(&chars, &mut i)?;
        skip_spaces(&chars, &mut i);
        if i != chars.len() {
            return None;
        }
        Some(title)
    };

    Some((label, LinkReference { dest, title }))
}

fn parse_reference_title_line(line: &str) -> Option<String> {
    if let Some(title) = parse_simple_ascii_reference_title_line(line) {
        return Some(title);
    }

    let t = trim_start_space_tab(line);
    if t.is_empty() {
        return None;
    }
    let chars: Vec<char> = t.chars().collect();
    let mut i = 0usize;
    let title = parse_link_title(&chars, &mut i)?;
    skip_spaces(&chars, &mut i);
    (i == chars.len()).then_some(title)
}

fn parse_simple_ascii_reference_definition(line: &str) -> Option<(String, LinkReference)> {
    if leading_spaces(line) > 3 || !line.is_ascii() || line.contains('&') || line.contains('\\') {
        return None;
    }
    let t = trim_start_space_tab(line);
    let bytes = t.as_bytes();
    if bytes.first() != Some(&b'[') {
        return None;
    }

    let close = bytes.iter().position(|&byte| byte == b']')?;
    let label = normalize_simple_ascii_reference_label(&t[1..close])?;
    if bytes.get(close + 1) != Some(&b':') {
        return None;
    }
    let mut i = close + 2;
    skip_ascii_spaces(bytes, &mut i);
    if i >= bytes.len() {
        return None;
    }

    let dest = if bytes[i] == b'<' {
        i += 1;
        let start = i;
        while i < bytes.len() && bytes[i] != b'>' {
            if bytes[i] == b'<' {
                return None;
            }
            i += 1;
        }
        if i >= bytes.len() || i == start {
            return None;
        }
        let dest = t[start..i].to_string();
        i += 1;
        dest
    } else {
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            // The general destination parser owns parenthesis balance and
            // rejects unescaped '<' and controls. Do not let this ASCII fast
            // path turn malformed source into a definition and delete it.
            if matches!(bytes[i], b'(' | b')' | b'<') || bytes[i].is_ascii_control() {
                return None;
            }
            i += 1;
        }
        if i == start {
            return None;
        }
        t[start..i].to_string()
    };

    let spaces_start = i;
    skip_ascii_spaces(bytes, &mut i);
    let title = if i >= bytes.len() {
        None
    } else {
        if i == spaces_start {
            return None;
        }
        let close_ch = match bytes[i] {
            b'"' => b'"',
            b'\'' => b'\'',
            b'(' => b')',
            _ => return None,
        };
        i += 1;
        let start = i;
        while i < bytes.len() && bytes[i] != close_ch {
            if bytes[i] == b'\\' || (close_ch == b')' && bytes[i] == b'(') {
                return None;
            }
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        let title = t[start..i].to_string();
        i += 1;
        skip_ascii_spaces(bytes, &mut i);
        if i != bytes.len() {
            return None;
        }
        Some(title)
    };

    Some((label, LinkReference { dest, title }))
}

fn parse_simple_ascii_reference_title_line(line: &str) -> Option<String> {
    if !line.is_ascii() || line.contains('&') {
        return None;
    }
    let t = trim_start_space_tab(line);
    if t.is_empty() {
        return None;
    }
    let bytes = t.as_bytes();
    let close_ch = match bytes.first().copied()? {
        b'"' => b'"',
        b'\'' => b'\'',
        b'(' => b')',
        _ => return None,
    };
    let mut i = 1usize;
    let start = i;
    while i < bytes.len() && bytes[i] != close_ch {
        if bytes[i] == b'\\' || (close_ch == b')' && bytes[i] == b'(') {
            return None;
        }
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let title = t[start..i].to_string();
    i += 1;
    skip_ascii_spaces(bytes, &mut i);
    (i == bytes.len()).then_some(title)
}

fn normalize_simple_ascii_reference_label(label: &str) -> Option<String> {
    if label.len() > MAX_REFERENCE_LABEL_LEN {
        return None;
    }
    if label.as_bytes().iter().any(|&byte| {
        matches!(byte, b'[' | b']' | b'\\')
            || !byte.is_ascii()
            || (byte.is_ascii_whitespace() && !matches!(byte, b' ' | b'\t'))
    }) {
        return None;
    }
    let trimmed = trim_space_tab(label);
    if trimmed.is_empty() {
        return None;
    }

    let mut out = String::with_capacity(trimmed.len());
    let mut pending_space = false;
    for byte in trimmed.bytes() {
        if matches!(byte, b' ' | b'\t') {
            pending_space = true;
            continue;
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        out.push(char::from(byte.to_ascii_lowercase()));
        pending_space = false;
    }
    Some(out)
}

fn skip_ascii_spaces(bytes: &[u8], i: &mut usize) {
    while bytes
        .get(*i)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        *i += 1;
    }
}

// ---- block detectors --------------------------------------------------------

fn atx_heading(line: &str) -> Option<(u8, &str)> {
    let indent = leading_spaces(line);
    if indent > 3 {
        return None;
    }
    atx_heading_after_indent(&line[indent..])
}

fn atx_heading_after_indent(t: &str) -> Option<(u8, &str)> {
    let hashes = t.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &t[hashes..];
    if !rest.is_empty() && !starts_space_or_tab(rest) {
        return None; // `#text` is not a heading
    }
    let content = atx_heading_content(rest);
    Some((hashes as u8, content))
}

fn atx_heading_content(rest: &str) -> &str {
    let raw = trim_end_space_tab(rest);
    let bytes = raw.as_bytes();
    let mut hash_start = bytes.len();
    while hash_start > 0 && bytes[hash_start - 1] == b'#' {
        hash_start -= 1;
    }

    if hash_start < bytes.len() && hash_start > 0 && is_space_or_tab_byte(bytes[hash_start - 1]) {
        return trim_space_tab(&raw[..hash_start]);
    }

    trim_space_tab(raw)
}

fn setext_underline(line: &str) -> Option<u8> {
    if leading_spaces(line) > 3 {
        return None;
    }
    setext_underline_trimmed(trim_space_tab(line))
}

fn setext_underline_trimmed(t: &str) -> Option<u8> {
    let first = t.chars().next()?;
    let level = match first {
        '=' => 1,
        '-' => 2,
        _ => return None,
    };
    let marker_len = t.chars().take_while(|&c| c == first).count();
    if marker_len > 0 && t[marker_len..].chars().all(is_space_or_tab) {
        Some(level)
    } else {
        None
    }
}

fn is_thematic_break(line: &str) -> bool {
    if leading_spaces(line) > 3 {
        return false;
    }
    thematic_break_trimmed(trim_space_tab(line))
}

fn thematic_break_trimmed(t: &str) -> bool {
    if t.len() < 3 {
        return false;
    }
    for ch in ['-', '*', '_'] {
        if t.chars().all(|c| c == ch || c == ' ' || c == '\t')
            && t.chars().filter(|&c| c == ch).count() >= 3
        {
            return true;
        }
    }
    false
}

fn open_fence(line: &str) -> Option<(char, usize, &str)> {
    let indent = leading_spaces(line);
    if indent > 3 {
        return None;
    }
    open_fence_after_indent(&line[indent..])
}

fn open_fence_after_indent(t: &str) -> Option<(char, usize, &str)> {
    let marker = match t.as_bytes().first().copied()? {
        b'`' => b'`',
        b'~' => b'~',
        _ => return None,
    };
    let n = t.bytes().take_while(|&byte| byte == marker).count();
    if n < 3 {
        return None;
    }
    let info = &t[n..];
    if marker == b'`' && info.contains('`') {
        return None;
    }
    Some((marker as char, n, info))
}

fn decode_info_string(info: &str) -> String {
    let chars: Vec<char> = info.chars().collect();
    let mut out = String::with_capacity(info.len());
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() && is_ascii_punct(chars[i + 1]) {
            out.push(chars[i + 1]);
            i += 2;
        } else if chars[i] == '&' {
            if let Some((decoded, next)) = parse_character_reference(&chars, i) {
                out.push_str(&decoded);
                i = next;
            } else {
                out.push('&');
                i += 1;
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn is_close_fence(line: &str, ch: char, len: usize) -> bool {
    let indent = leading_spaces(line);
    if indent > 3 {
        return false;
    }
    let t = &line[indent..];
    let marker_len = t.chars().take_while(|&c| c == ch).count();
    marker_len >= len && t[marker_len..].chars().all(is_space_or_tab)
}

#[derive(Clone, Copy)]
enum MathBlockStart<'a> {
    SingleLine(&'a str),
    Multiline(&'a str),
}

/// Display-math delimiters are shared by AST assembly, reference collection,
/// table admission, and container ownership. A marker in paragraph text is
/// inline syntax; callers decide whether this line is at a block boundary.
fn math_block_start(line: &str) -> Option<MathBlockStart<'_>> {
    if leading_spaces(line) >= 4 {
        return None;
    }
    let after = trim_unicode_ws(line).strip_prefix("$$")?;
    if let Some(content) = after.strip_suffix("$$") {
        Some(MathBlockStart::SingleLine(trim_unicode_ws(content)))
    } else if !after.contains("$$") {
        Some(MathBlockStart::Multiline(trim_unicode_ws(after)))
    } else {
        None
    }
}

fn math_block_close(line: &str) -> Option<&str> {
    trim_unicode_ws(line)
        .strip_suffix("$$")
        .map(trim_end_unicode_ws)
}

fn math_block_extent(lines: &[&str], start: MathBlockStart<'_>) -> usize {
    if matches!(start, MathBlockStart::SingleLine(_)) {
        return 1;
    }
    lines[1..]
        .iter()
        .position(|line| math_block_close(line).is_some())
        .map_or(lines.len(), |index| index + 2)
}

fn indented_code_start(line: &str) -> bool {
    leading_spaces(line) >= 4
}

fn parse_indented_code(lines: &[&str], profiler: &ParseProfiler) -> (String, usize) {
    let used = indented_code_extent(lines, |line| *line);
    // Presize the body from the extent: raw byte lengths plus one newline per
    // line. Blank lines push only '\n' and content lines drop at most four
    // leading columns, so the estimate never under-grows; capacity is
    // invisible to the collected bytes.
    let body_bytes = lines
        .iter()
        .take(used)
        .map(|line| line.len() + 1)
        .sum::<usize>();
    let mut code = String::with_capacity(body_bytes);
    let mut code_parts = Vec::new();
    for line in lines.iter().take(used) {
        let part = strip_n(line, 4);
        code_parts.push((code.len(), part));
        code.push_str(part);
        code.push('\n');
    }
    register_parts(&code, &code_parts, profiler);
    (code, used)
}

fn indented_code_extent<T>(lines: &[T], text: impl Fn(&T) -> &str) -> usize {
    let mut i = 0usize;
    while i < lines.len() {
        let line = text(&lines[i]);
        if is_blank_line(line) {
            let mut next = i + 1;
            while next < lines.len() && is_blank_line(text(&lines[next])) {
                next += 1;
            }
            if next >= lines.len() || !indented_code_start(text(&lines[next])) {
                break;
            }
            i += 1;
            continue;
        }
        if !indented_code_start(line) {
            break;
        }
        i += 1;
    }
    i
}

/// The content of a `>`-quoted line with the marker and one optional following
/// space removed, borrowed from the input (no allocation).
fn strip_blockquote_marker(line: &str) -> std::borrow::Cow<'_, str> {
    let Some(marker) = blockquote_marker_offset(line) else {
        return std::borrow::Cow::Borrowed(line);
    };
    let rest = &line[marker + 1..];
    if let Some(after_tab) = rest.strip_prefix('\t') {
        let col = marker + 1;
        let tab_width = 4 - (col % 4);
        let spaces_left = tab_width - 1;
        let expanded = expand_leading_tabs_with_col(after_tab, col + tab_width);
        let mut s = String::with_capacity(spaces_left + expanded.len());
        for _ in 0..spaces_left {
            s.push(' ');
        }
        s.push_str(expanded.as_ref());
        std::borrow::Cow::Owned(s)
    } else {
        std::borrow::Cow::Borrowed(rest.strip_prefix(' ').unwrap_or(rest))
    }
}

fn blockquote_marker_offset(line: &str) -> Option<usize> {
    let indent = leading_spaces(line);
    (indent <= 3 && line.as_bytes().get(indent) == Some(&b'>')).then_some(indent)
}

fn blockquote_marker_start(line: &str) -> bool {
    blockquote_marker_offset(line).is_some()
}

/// Strip one blockquote container, keeping exactly its marked lines and lazy
/// continuations of an open paragraph. The current leaf block, rather than the
/// previous physical line, owns laziness: code/HTML bodies and completed blocks
/// must not capture unquoted prose or reference definitions.
///
/// Both semantic parsing and reference collection use this split. Copied tab
/// expansion and escaped lazy markers retain their original source coordinates.
fn split_blockquote_lines_tracked<'a>(
    lines: &[&'a str],
    mut tracker: Option<&mut destinations::DestinationTracker>,
) -> (Vec<std::borrow::Cow<'a, str>>, usize) {
    let mut inner = Vec::new();
    let mut tail = ContainerTail::default();
    let mut used = 0;
    for &line in lines {
        let stripped = if blockquote_marker_start(line) {
            let stripped = strip_blockquote_marker(line);
            if let std::borrow::Cow::Owned(copied) = &stripped
                && let Some(tracker) = &mut tracker
            {
                tracker.map_suffix(copied, line);
            }
            stripped
        } else if tail.can_continue_lazily(line) {
            // Removing the container indentation must not promote a lazy line
            // into a setext heading or a list marker inside the collected body.
            let trimmed = trim_start_space_tab(line);
            if setext_underline(trimmed).is_some()
                || (list_marker(trimmed).is_some()
                    && (leading_spaces(line) >= 4 || list_marker(line).is_none()))
            {
                let copied = format!("\\{trimmed}");
                if let Some(tracker) = &mut tracker {
                    tracker.map_suffix(&copied, trimmed);
                }
                std::borrow::Cow::Owned(copied)
            } else {
                std::borrow::Cow::Borrowed(trimmed)
            }
        } else {
            break;
        };
        tail.observe(stripped.as_ref());
        inner.push(stripped);
        used += 1;
    }
    (inner, used)
}

/// The end condition for a started HTML block (CommonMark block types 1-7).
#[derive(Clone, Copy)]
enum HtmlBlockEnd {
    /// Types 1-5: continue until (and including) the first line that contains
    /// one of these end markers — even across blank lines.
    Marker(&'static [&'static str]),
    /// Type 6: continue until the next blank line; can interrupt a paragraph.
    Blank,
    /// Type 7: the same end condition, but only starts at a block boundary.
    BlankNonInterrupting,
}

impl HtmlBlockEnd {
    fn interrupts_paragraph(self) -> bool {
        !matches!(self, Self::BlankNonInterrupting)
    }
}

/// Classify an HTML block start and return its end condition, or `None` when the
/// line does not begin an HTML block. Types 1-5 end at a literal closing token
/// (`-->`, `</pre>`, ...) that may sit on a later line; types 6-7 end at a blank
/// line. The previous implementation blank-terminated *every* type, which split
/// `<pre>`/comment blocks at the first blank line and emitted unterminated tags.
fn html_block_kind(line: &str) -> Option<HtmlBlockEnd> {
    let indent = leading_spaces(line);
    if indent > 3 {
        return None;
    }
    html_block_kind_from_block_start(&line[indent..])
}

fn html_block_kind_from_block_start(t: &str) -> Option<HtmlBlockEnd> {
    if !t.starts_with('<') {
        return None;
    }
    // Type 2: comment.
    if t.starts_with("<!--") {
        return Some(HtmlBlockEnd::Marker(&["-->"]));
    }
    // Type 5: CDATA section.
    if t.starts_with("<![CDATA[") {
        return Some(HtmlBlockEnd::Marker(&["]]>"]));
    }
    // Type 3: processing instruction.
    if t.starts_with("<?") {
        return Some(HtmlBlockEnd::Marker(&["?>"]));
    }
    // Type 4: declaration `<!` + ASCII letter (e.g. `<!DOCTYPE html>`).
    if t.starts_with("<!") {
        return if t.as_bytes().get(2).is_some_and(u8::is_ascii_alphabetic) {
            Some(HtmlBlockEnd::Marker(&[">"]))
        } else {
            None
        };
    }
    // Type 1: raw-text elements (`<script>`, `<pre>`, `<style>`, `<textarea>`).
    if let Some(markers) = html_raw_text_end_markers(t) {
        return Some(HtmlBlockEnd::Marker(markers));
    }
    // Type 6 requires a tag-name boundary, though not a complete tag. A prefix
    // such as `<div.foo>` or `<div/invalid>` must remain ordinary Markdown.
    let name = html_tag_name(t)?;
    let prefix_len = if t.starts_with("</") { 2 } else { 1 };
    let after = &t[prefix_len + name.len()..];
    if is_html_block_tag(name)
        && (matches!(after.as_bytes().first(), None | Some(b' ' | b'\t' | b'>'))
            || after.starts_with("/>"))
    {
        return Some(HtmlBlockEnd::Blank);
    }
    // Type 7 accepts a complete open/closing tag on its own line, and cannot
    // interrupt an existing paragraph. Consumers must preserve that distinction.
    html_blocks::complete_tag_line(t).then_some(HtmlBlockEnd::BlankNonInterrupting)
}

fn html_block_start(line: &str) -> bool {
    html_block_kind(line).is_some()
}

/// End markers for CommonMark type-1 raw-text HTML blocks. A start matches
/// `<name` (case-insensitive) followed by space, tab, `>`, or end of line. Any of
/// the four raw-text closing tags ends this block, regardless of its opener.
fn html_raw_text_end_markers(t: &str) -> Option<&'static [&'static str]> {
    const RAW: [&str; 4] = ["script", "pre", "style", "textarea"];
    const END: &[&str] = &["</script>", "</pre>", "</style>", "</textarea>"];
    let rest = t.strip_prefix('<')?;
    for name in RAW {
        if let Some(after) = strip_ascii_prefix_ignore_case(rest, name)
            && matches!(after.as_bytes().first(), None | Some(b' ' | b'\t' | b'>'))
        {
            return Some(END);
        }
    }
    None
}

/// Given an HTML block that starts at `start`, return the exclusive end line
/// index per its `end` condition. `text` extracts the raw text of a line.
fn html_block_end<T>(
    lines: &[T],
    start: usize,
    end: HtmlBlockEnd,
    text: impl Fn(&T) -> &str,
) -> usize {
    match end {
        HtmlBlockEnd::Marker(markers) => {
            let mut k = start;
            while k < lines.len() {
                let hit = markers
                    .iter()
                    .any(|marker| contains_ascii_ignore_case(text(&lines[k]), marker));
                k += 1;
                if hit {
                    break;
                }
            }
            k
        }
        HtmlBlockEnd::Blank | HtmlBlockEnd::BlankNonInterrupting => {
            let mut k = start + 1;
            while k < lines.len() && !is_blank_line(text(&lines[k])) {
                k += 1;
            }
            k
        }
    }
}

fn html_tag_name(t: &str) -> Option<&str> {
    let rest = t.strip_prefix("</").or_else(|| t.strip_prefix('<'))?;
    let mut end = 0usize;
    for (idx, ch) in rest.char_indices() {
        if idx == 0 && !ch.is_ascii_alphabetic() {
            return None;
        }
        if ch.is_ascii_alphanumeric() || ch == '-' {
            end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 { None } else { Some(&rest[..end]) }
}

fn is_html_block_tag(name: &str) -> bool {
    if name.bytes().any(|b| b.is_ascii_uppercase()) {
        is_html_block_tag_lowercase(&name.to_ascii_lowercase())
    } else {
        is_html_block_tag_lowercase(name)
    }
}

fn is_html_block_tag_lowercase(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "base"
            | "basefont"
            | "blockquote"
            | "body"
            | "caption"
            | "center"
            | "col"
            | "colgroup"
            | "dd"
            | "details"
            | "dialog"
            | "dir"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "frame"
            | "frameset"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "head"
            | "header"
            | "hr"
            | "html"
            | "iframe"
            | "legend"
            | "li"
            | "link"
            | "main"
            | "menu"
            | "menuitem"
            | "nav"
            | "noframes"
            | "ol"
            | "optgroup"
            | "option"
            | "p"
            | "param"
            | "search"
            | "section"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "title"
            | "tr"
            | "track"
            | "ul"
    )
}

fn strip_ascii_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let prefix = prefix.as_bytes();
    let bytes = s.as_bytes();
    if bytes.len() >= prefix.len() && bytes[..prefix.len()].eq_ignore_ascii_case(prefix) {
        s.get(prefix.len()..)
    } else {
        None
    }
}

fn contains_ascii_ignore_case(haystack: &str, needle: &str) -> bool {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    if needle.len() == 1 {
        return haystack
            .iter()
            .any(|byte| byte.eq_ignore_ascii_case(&needle[0]));
    }

    let first = needle[0];
    let last_start = haystack.len() - needle.len();
    let mut start = 0usize;
    while start <= last_start {
        if haystack[start].eq_ignore_ascii_case(&first)
            && haystack[start..start + needle.len()].eq_ignore_ascii_case(needle)
        {
            return true;
        }
        start += 1;
    }
    false
}

fn is_definition_marker(line: &str) -> bool {
    let trimmed = trim_start_space_tab(line);
    leading_spaces(line) < 4
        && (trimmed.starts_with(": ") || trimmed.starts_with(":\t") || trimmed == ":")
}

fn strip_definition_marker(line: &str) -> &str {
    let trimmed = trim_start_space_tab(line);
    if let Some(rest) = trimmed.strip_prefix(": ") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix(":\t") {
        rest
    } else if trimmed == ":" {
        ""
    } else {
        trimmed
    }
}

fn parse_definition_list_profiled(
    lines: &[&str],
    refs: &ReferenceMap,
    profiler: &mut ParseProfiler,
) -> Option<(Block, usize)> {
    if lines.len() < 2 {
        return None;
    }

    let mut items: Vec<franken_markdown::ast::DefinitionItem> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let mut term_lines = Vec::new();
        let item_start = i;
        while i < lines.len() {
            let line = lines[i];
            let trimmed = trim_space_tab(line);
            if trimmed.is_empty() {
                break;
            }
            let scan = BlockStartScan::new(line, trimmed);
            if scanned_paragraph_interrupt(scan) {
                break;
            }
            if is_definition_marker(line) {
                break;
            }
            term_lines.push(trimmed);
            i += 1;
        }

        if term_lines.is_empty() || i >= lines.len() || !is_definition_marker(lines[i]) {
            if items.is_empty() {
                return None;
            }
            i = item_start;
            break;
        }

        let terms: Vec<Vec<franken_markdown::ast::Inline>> = term_lines
            .into_iter()
            .map(|t| parse_inlines_with_refs_profiled(t, refs, profiler))
            .collect();

        let mut definitions: Vec<Vec<franken_markdown::ast::Inline>> = Vec::new();
        while i < lines.len() && is_definition_marker(lines[i]) {
            let first_def = strip_definition_marker(lines[i]);
            let checkpoint = profiler.destination_checkpoint();
            let mut parts = profiler
                .destinations
                .is_some()
                .then(|| vec![(0, first_def)]);
            let mut def_text = first_def.to_string();
            i += 1;
            while i < lines.len() {
                let line = lines[i];
                let trimmed = trim_space_tab(line);
                if trimmed.is_empty() {
                    break;
                }
                let scan = BlockStartScan::new(line, trimmed);
                if scanned_paragraph_interrupt(scan) {
                    break;
                }
                if is_definition_marker(line) {
                    break;
                }
                if i + 1 < lines.len() && is_definition_marker(lines[i + 1]) {
                    break;
                }
                if !def_text.is_empty() {
                    def_text.push(' ');
                }
                if let Some(parts) = &mut parts {
                    parts.push((def_text.len(), trimmed));
                }
                def_text.push_str(trimmed);
                i += 1;
            }
            if let Some(parts) = parts {
                profiler.destination_parts(&def_text, &parts);
            }
            definitions.push(parse_inlines_with_refs_profiled(&def_text, refs, profiler));
            profiler.destination_restore(checkpoint);
        }

        if definitions.is_empty() {
            if items.is_empty() {
                return None;
            }
            i = item_start;
            break;
        }

        items.push(franken_markdown::ast::DefinitionItem { terms, definitions });

        if i < lines.len() && trim_space_tab(lines[i]).is_empty() {
            let mut next_k = i;
            while next_k < lines.len() && trim_space_tab(lines[next_k]).is_empty() {
                next_k += 1;
            }
            let mut term_cand = next_k;
            while term_cand < lines.len()
                && !trim_space_tab(lines[term_cand]).is_empty()
                && !is_definition_marker(lines[term_cand])
            {
                let scan = BlockStartScan::new(lines[term_cand], trim_space_tab(lines[term_cand]));
                if scanned_paragraph_interrupt(scan) {
                    break;
                }
                term_cand += 1;
            }
            if term_cand > next_k
                && term_cand < lines.len()
                && is_definition_marker(lines[term_cand])
            {
                i = next_k;
            } else {
                break;
            }
        }
    }

    if items.is_empty() {
        None
    } else {
        Some((Block::DefinitionList(items), i))
    }
}

// ---- lists ------------------------------------------------------------------

#[derive(Clone)]
struct Marker<'a> {
    indent: usize,
    ordered: bool,
    start: u64,
    marker_char: char,
    content_indent: usize,
    rest: std::borrow::Cow<'a, str>,
}

fn list_marker(line: &str) -> Option<Marker<'_>> {
    let indent = leading_spaces(line);
    if indent > 3 || is_thematic_break(line) {
        return None;
    }
    let t = trim_start_space_tab(line);
    if let Some(first) = t.chars().next()
        && (first == '-' || first == '*' || first == '+')
    {
        let after_marker = &t[first.len_utf8()..];
        let marker_cols = 1usize;
        let (rest, padding) = marker_padding(after_marker, indent + marker_cols)?;
        return Some(Marker {
            indent,
            ordered: false,
            start: 1,
            marker_char: first,
            content_indent: indent + marker_cols + padding,
            rest,
        });
    }
    // Ordered: digits then `.` or `)` then space.
    let digit_len = t.bytes().take_while(u8::is_ascii_digit).count();
    if digit_len > 0 && digit_len <= 9 {
        let digits = &t[..digit_len];
        let after = &t[digit_len..];
        if (after.starts_with('.') || after.starts_with(')'))
            && let Ok(start) = digits.parse()
            && let Some((rest, padding)) = marker_padding(&after[1..], indent + digit_len + 1)
        {
            let delim = after.chars().next()?;
            return Some(Marker {
                indent,
                ordered: true,
                start,
                marker_char: delim,
                content_indent: indent + digit_len + 1 + padding,
                rest,
            });
        }
    }
    None
}

fn marker_padding<'a>(
    after_marker: &'a str,
    column_after_marker: usize,
) -> Option<(std::borrow::Cow<'a, str>, usize)> {
    if after_marker.is_empty() {
        return Some((std::borrow::Cow::Borrowed(""), 1));
    }
    let first = after_marker.chars().next()?;
    if first == '\t' {
        let tab_width = 4 - (column_after_marker % 4);
        let spaces_left = tab_width - 1;
        let expanded =
            expand_leading_tabs_with_col(&after_marker[1..], column_after_marker + tab_width);
        let mut s = String::with_capacity(spaces_left + expanded.len());
        for _ in 0..spaces_left {
            s.push(' ');
        }
        s.push_str(expanded.as_ref());
        return Some((std::borrow::Cow::Owned(s), 1));
    }
    if first == ' ' {
        let space_count = after_marker.bytes().take_while(|&b| b == b' ').count();
        if space_count == after_marker.len() {
            return Some((std::borrow::Cow::Borrowed(""), 1));
        }
        if space_count <= 4 {
            return Some((
                std::borrow::Cow::Borrowed(&after_marker[space_count..]),
                space_count,
            ));
        }
        return Some((std::borrow::Cow::Borrowed(&after_marker[1..]), 1));
    }
    None
}

fn list_marker_interrupts_paragraph(line: &str) -> bool {
    list_marker(line)
        .as_ref()
        .is_some_and(marker_interrupts_paragraph)
}

fn marker_interrupts_paragraph(marker: &Marker<'_>) -> bool {
    // CommonMark: a list interrupts a paragraph only when the first item has
    // content on the marker line, is indented <= 3 spaces, and (for ordered lists) starts at 1.
    if marker.indent > 3 || marker.rest.is_empty() {
        return false;
    }
    !marker.ordered || marker.start == 1
}

/// A list split into per-item bodies (marker/indent stripped, task marker
/// separated) without parsing them, plus the list flags and the number of lines
/// consumed. Body lines borrow from the source wherever possible: marker rests,
/// content-indent strips, and lazy continuations are all slices of the original
/// input; inserted structural blanks use the shared empty string. Shared by
/// `parse_list_profiled_with_first_marker` (which parses each body) and
/// reference collection (which recurses into each body to find nested
/// definitions), so the two agree exactly on item boundaries.
struct ListSplit<'a> {
    ordered: bool,
    start: u64,
    tight: bool,
    /// `(task marker, body lines)` per item.
    items: Vec<(Option<bool>, Vec<std::borrow::Cow<'a, str>>)>,
    used: usize,
}

#[derive(Clone, Copy)]
enum TailContainer {
    Quote,
    List {
        indent: usize,
        content_indent: usize,
        ordered: bool,
    },
}

#[derive(Default)]
enum TailLeaf {
    #[default]
    Boundary,
    Paragraph {
        table_columns: Option<usize>,
        table_has_pipe: bool,
    },
    Fence {
        ch: char,
        len: usize,
    },
    IndentedCode,
    Html(HtmlBlockEnd),
    Math,
    Table(TableBodyState),
    Footnote,
}

/// Track the current leaf block while a quote or list item is collected. Laziness is
/// permitted only for an open paragraph, including one inside nested lists or
/// quotes. Looking only at the previous line mistakes fenced/HTML/code content
/// for prose, while looking only at indentation rejects hanging paragraphs.
/// Each line visits at most the parser's bounded container depth; no accumulated
/// container body is reparsed to decide whether a lazy line belongs to it.
#[derive(Default)]
struct ContainerTail {
    containers: Vec<TailContainer>,
    leaf: TailLeaf,
    // Follow the definition grammar incrementally, without retaining and
    // reparsing an ever-growing prefix of a long title. Definitions are removed
    // after collection; their remaining text can still continue lazily.
    reference_scan: Option<ReferenceDefinitionScan>,
}

impl ContainerTail {
    fn can_continue_lazily(&self, line: &str) -> bool {
        matches!(self.leaf, TailLeaf::Paragraph { .. })
            && !is_blank_line(line)
            && !scanned_paragraph_interrupt(BlockStartScan::new(line, trim_space_tab(line)))
    }

    fn is_literal(&self) -> bool {
        matches!(
            self.leaf,
            TailLeaf::Fence { .. } | TailLeaf::IndentedCode | TailLeaf::Html(_) | TailLeaf::Math
        )
    }

    fn retains_literal_blank(&self) -> bool {
        matches!(
            self.leaf,
            TailLeaf::Fence { .. } | TailLeaf::Html(HtmlBlockEnd::Marker(_)) | TailLeaf::Math
        ) && !self
            .containers
            .iter()
            .any(|container| matches!(container, TailContainer::Quote))
    }

    fn observe(&mut self, line: &str) {
        self.observe_at(line, 0);
    }

    fn observe_at(&mut self, line: &str, depth: usize) {
        if let Some(container) = self.containers.get(depth).copied() {
            let sibling = match container {
                TailContainer::Quote => {
                    if blockquote_marker_start(line) {
                        self.observe_at(strip_blockquote_marker(line).as_ref(), depth + 1);
                        return;
                    }
                    false
                }
                TailContainer::List {
                    indent,
                    content_indent,
                    ordered,
                } => {
                    if is_blank_line(line) || leading_spaces(line) >= content_indent {
                        self.observe_at(strip_n(line, content_indent), depth + 1);
                        return;
                    }
                    list_marker(line).is_some_and(|next| {
                        (next.indent == indent && next.ordered == ordered)
                            || marker_interrupts_paragraph(&next)
                    })
                }
            };
            if !sibling && self.can_continue_lazily(line) {
                return;
            }
            self.containers.truncate(depth);
            self.leaf = TailLeaf::Boundary;
            self.reference_scan = None;
        }

        if let Some(scan) = &mut self.reference_scan {
            match scan.line(line) {
                Some(ReferenceProgress::Pending) => {}
                Some(progress) => {
                    self.leaf = TailLeaf::Paragraph {
                        table_columns: None,
                        table_has_pipe: false,
                    };
                    if progress == ReferenceProgress::Title {
                        self.reference_scan = None;
                    }
                    return;
                }
                None => self.reference_scan = None,
            }
        }

        match self.leaf {
            TailLeaf::Fence { ch, len } => {
                if is_close_fence(line, ch, len) {
                    self.leaf = TailLeaf::Boundary;
                }
                return;
            }
            TailLeaf::Html(end) => {
                let closed = match end {
                    HtmlBlockEnd::Marker(markers) => markers
                        .iter()
                        .any(|marker| contains_ascii_ignore_case(line, marker)),
                    HtmlBlockEnd::Blank | HtmlBlockEnd::BlankNonInterrupting => is_blank_line(line),
                };
                if closed {
                    self.leaf = TailLeaf::Boundary;
                }
                return;
            }
            TailLeaf::Math => {
                if math_block_close(line).is_some() {
                    self.leaf = TailLeaf::Boundary;
                }
                return;
            }
            TailLeaf::IndentedCode | TailLeaf::Footnote
                if is_blank_line(line) || leading_spaces(line) >= 4 =>
            {
                return;
            }
            TailLeaf::Table(mut body) if is_table_row_source(line) => {
                self.leaf = if body.admit_columns(split_table_row_cell_count(line)) {
                    TailLeaf::Table(body)
                } else {
                    // Match the parser's lossless padding fallback, including
                    // its refusal to promote this row into a definition/header.
                    TailLeaf::Paragraph {
                        table_columns: None,
                        table_has_pipe: false,
                    }
                };
                return;
            }
            TailLeaf::Paragraph {
                table_columns,
                table_has_pipe,
            } if !is_blank_line(line) => {
                if let Some(columns) = table_columns
                    && (table_has_pipe || line.contains('|'))
                    && Some(columns) == validated_table_delimiter_cell_count(line)
                {
                    self.leaf = TailLeaf::Table(TableBodyState::new(columns));
                    return;
                }
                if setext_underline(line).is_some() {
                    self.leaf = TailLeaf::Boundary;
                    return;
                }
                if !scanned_paragraph_interrupt(BlockStartScan::new(line, trim_space_tab(line))) {
                    self.leaf = TailLeaf::Paragraph {
                        table_columns: None,
                        table_has_pipe: false,
                    };
                    return;
                }
            }
            _ => {}
        }
        self.leaf = TailLeaf::Boundary;
        if is_blank_line(line) || is_thematic_break(line) || atx_heading(line).is_some() {
            return;
        }
        if let Some((ch, len, _)) = open_fence(line) {
            self.leaf = TailLeaf::Fence { ch, len };
        } else if let Some(start) = math_block_start(line) {
            if matches!(start, MathBlockStart::Multiline(_)) {
                self.leaf = TailLeaf::Math;
            }
        } else if indented_code_start(line) {
            self.leaf = TailLeaf::IndentedCode;
        } else if footnote_definition_marker(line) {
            self.leaf = TailLeaf::Footnote;
        } else if blockquote_marker_start(line) {
            if depth < MAX_BLOCK_NESTING_DEPTH {
                self.reference_scan = None;
                self.containers.push(TailContainer::Quote);
                self.observe_at(strip_blockquote_marker(line).as_ref(), depth + 1);
            }
        } else if let Some(end) = html_block_kind(line) {
            self.leaf = TailLeaf::Html(end);
            // A comment or raw-text element may close on its opening line.
            if matches!(end, HtmlBlockEnd::Marker(_)) {
                self.observe_at(line, depth);
            }
        } else if let Some(marker) = list_marker(line) {
            if depth < MAX_BLOCK_NESTING_DEPTH {
                self.reference_scan = None;
                self.containers.push(TailContainer::List {
                    indent: marker.indent,
                    content_indent: marker.content_indent,
                    ordered: marker.ordered,
                });
                self.observe_at(split_task_marker(marker.rest.as_ref()).1, depth + 1);
            }
        } else {
            if self.reference_scan.is_none() && trim_start_space_tab(line).starts_with('[') {
                let mut scan = ReferenceDefinitionScan::new();
                if let Some(progress) = scan.line(line) {
                    if progress != ReferenceProgress::Title {
                        self.reference_scan = Some(scan);
                    }
                    self.leaf = TailLeaf::Paragraph {
                        table_columns: None,
                        table_has_pipe: false,
                    };
                    return;
                }
            }
            self.leaf = TailLeaf::Paragraph {
                table_columns: is_table_row_source(line).then(|| split_table_row_cell_count(line)),
                table_has_pipe: line.contains('|'),
            };
        }
    }
}

fn split_list_items_with_first_marker_tracked<'a>(
    lines: &[&'a str],
    first: Marker<'a>,
    mut tracker: Option<&mut destinations::DestinationTracker>,
) -> ListSplit<'a> {
    let ordered = first.ordered;
    let start = first.start;
    let mut items: Vec<(Option<bool>, Vec<std::borrow::Cow<'a, str>>)> = Vec::new();
    let mut tight = true;
    let mut i = 0;
    while i < lines.len() {
        let marker = if i == 0 {
            Some(first.clone())
        } else {
            list_marker(lines[i])
        };
        let Some(m) = marker.filter(|m| m.ordered == ordered && m.marker_char == first.marker_char)
        else {
            break;
        };
        let item_origin = lines[i];
        let mut item_lines: Vec<std::borrow::Cow<'a, str>> = vec![m.rest.clone()];
        let mut nested_marker_checked_lines = 0;
        let mut has_open_nested_list = false;
        let mut tail = ContainerTail::default();
        tail.observe(split_task_marker(m.rest.as_ref()).1);
        i += 1;

        if items.is_empty()
            && is_blank_line(m.rest.as_ref())
            && i < lines.len()
            && is_blank_line(lines[i])
        {
            let (task, first_body) = split_task_marker(item_lines[0].as_ref());
            item_lines[0] = std::borrow::Cow::Owned(first_body.to_string());
            if let Some(tracker) = &mut tracker {
                tracker.map_suffix(item_lines[0].as_ref(), item_origin);
            }
            items.push((task, item_lines));
            break;
        }

        while i < lines.len() {
            if is_blank_line(lines[i]) && tail.retains_literal_blank() {
                let stripped = if leading_spaces(lines[i]) >= m.content_indent {
                    strip_n(lines[i], m.content_indent)
                } else {
                    ""
                };
                tail.observe(stripped);
                item_lines.push(std::borrow::Cow::Borrowed(stripped));
                i += 1;
                continue;
            }

            if is_blank_line(lines[i]) {
                let mut j = i + 1;
                while j < lines.len() && is_blank_line(lines[j]) {
                    j += 1;
                }
                if j < lines.len()
                    && list_marker(lines[j]).is_some_and(|next| {
                        next.ordered == ordered
                            && next.marker_char == first.marker_char
                            && next.indent < m.content_indent
                    })
                {
                    tight = false;
                    i = j;
                    break;
                }
                // A blank line followed by a new DIRECT block of THIS item (a
                // second paragraph at the item's content column) makes the list
                // loose (CommonMark: an item holding two blank-separated blocks).
                // Require the post-blank line to sit at EXACTLY the content column
                // and to not be a list marker: deeper-indented content belongs to
                // a nested sub-list (whose own blank loosens it via recursion), a
                // marker continues a sub-list, and a dedent is a trailing blank —
                // none of those loosen THIS list.
                if j < lines.len() {
                    let next_indent = leading_spaces(lines[j]);
                    // The item only grows during collection. Check each stored
                    // line at most once instead of rescanning its entire history
                    // at every paragraph boundary in a long list item.
                    if !has_open_nested_list {
                        has_open_nested_list = item_lines[nested_marker_checked_lines..]
                            .iter()
                            .any(|line| list_marker(line.as_ref()).is_some());
                        nested_marker_checked_lines = item_lines.len();
                    }
                    if next_indent == m.content_indent
                        || (!has_open_nested_list && next_indent >= m.content_indent + 4)
                    {
                        tight = false;
                    }
                }
                tail.observe("");
                item_lines.push(std::borrow::Cow::Borrowed(""));
                i += 1;
                continue;
            }

            if let Some(next) = list_marker(lines[i]) {
                let is_item_break = if next.indent <= m.indent {
                    if next.indent == m.indent && next.ordered == ordered {
                        true
                    } else {
                        marker_interrupts_paragraph(&next)
                    }
                } else if next.indent < m.content_indent {
                    marker_interrupts_paragraph(&next)
                } else {
                    false
                };
                if is_item_break {
                    break;
                }
            }

            if is_thematic_break(lines[i]) && leading_spaces(lines[i]) <= m.indent {
                break;
            }

            if leading_spaces(lines[i]) >= m.content_indent {
                let stripped = strip_n(lines[i], m.content_indent);
                // A non-1-start ordered marker cannot interrupt a paragraph, so
                // after prose it would be lazily absorbed; a blank line forces it
                // into its own sub-list. But when the previous content line is
                // itself a list item, this marker is just the natural 2nd/3rd/...
                // item of an ordered sub-list (start 2, 3, ...) and must stay in
                // one tight list — do not split it.
                let prev_is_list_item = item_lines
                    .last()
                    .is_some_and(|prev| list_marker(prev.as_ref()).is_some());
                if !tail.is_literal()
                    && !prev_is_list_item
                    && list_marker(stripped)
                        .is_some_and(|marker| marker.ordered && marker.start != 1)
                    && item_lines
                        .last()
                        .is_some_and(|prev| !is_blank_line(prev.as_ref()))
                {
                    tail.observe("");
                    item_lines.push(std::borrow::Cow::Borrowed(""));
                }
                tail.observe(stripped);
                item_lines.push(std::borrow::Cow::Borrowed(stripped));
            } else if item_lines
                .last()
                .is_some_and(|prev| is_blank_line(prev.as_ref()))
            {
                // A blank line separates this unindented line from the item, so
                // there is no open paragraph to lazily continue: it begins a new
                // top-level block and ends the list. (CommonMark lazy continuation
                // only extends an *open* paragraph — never after a blank line.)
                break;
            } else {
                // CommonMark lazy continuation: an unindented, non-marker line
                // continues only an OPEN paragraph, never a code/HTML block or
                // completed heading, and cannot start a new block outside it.
                if !tail.can_continue_lazily(lines[i]) {
                    break;
                }
                let lazy = trim_start_space_tab(lines[i]);
                if setext_underline(lazy).is_some()
                    || (list_marker(lazy).is_some()
                        && (leading_spaces(lines[i]) >= 4 || list_marker(lines[i]).is_none()))
                {
                    let copied = format!("\\{lazy}");
                    if let Some(tracker) = &mut tracker {
                        tracker.map_suffix(&copied, lazy);
                    }
                    tail.observe(&copied);
                    item_lines.push(std::borrow::Cow::Owned(copied));
                } else {
                    tail.observe(lazy);
                    item_lines.push(std::borrow::Cow::Borrowed(lazy));
                }
            }
            i += 1;
        }

        let (task, first_body) = split_task_marker(item_lines[0].as_ref());
        item_lines[0] = std::borrow::Cow::Owned(first_body.to_string());
        if let Some(tracker) = &mut tracker {
            tracker.map_suffix(item_lines[0].as_ref(), item_origin);
        }
        items.push((task, item_lines));
    }
    ListSplit {
        ordered,
        start,
        tight,
        items,
        used: i,
    }
}

fn parse_list_profiled_with_first_marker(
    lines: &[&str],
    refs: &ReferenceMap,
    profiler: &mut ParseProfiler,
    first: Marker<'_>,
) -> (List, usize) {
    let checkpoint = profiler.destination_checkpoint();
    let split =
        split_list_items_with_first_marker_tracked(lines, first, profiler.destinations.as_mut());
    let parsed = parse_list_split(split, refs, profiler);
    profiler.destination_restore(checkpoint);
    parsed
}

fn parse_list_split(
    split: ListSplit<'_>,
    refs: &ReferenceMap,
    profiler: &mut ParseProfiler,
) -> (List, usize) {
    let mut items = Vec::with_capacity(split.items.len());
    for (task, body) in split.items {
        let str_refs: Vec<&str> = body.iter().map(|s| s.as_ref()).collect();
        items.push(ListItem {
            task,
            blocks: parse_blocks_bounded(&str_refs, refs, profiler),
        });
    }
    (
        List {
            ordered: split.ordered,
            start: split.start,
            tight: split.tight,
            items,
        },
        split.used,
    )
}

fn split_task_marker(text: &str) -> (Option<bool>, &str) {
    let trimmed = trim_start_space_tab(text);
    // GFM requires the checkbox to be followed by at least one whitespace
    // character (or to be the item's entire content). Without this, `[x]foo`
    // renders as a checkbox plus "foo", and — worse — a list item whose text is a
    // reference definition like `[x]: /url` is mangled into a checkbox with body
    // ": /url", silently losing the definition.
    let (checked, after) = if let Some(rest) = trimmed.strip_prefix("[ ]") {
        (false, rest)
    } else if let Some(rest) = trimmed
        .strip_prefix("[x]")
        .or_else(|| trimmed.strip_prefix("[X]"))
    {
        (true, rest)
    } else {
        return (None, text);
    };
    match after.chars().next() {
        // A bare checkbox that is the entire item (e.g. `- [x]`).
        None => (Some(checked), after),
        // Consume exactly one whitespace separator, as the old prefixes did.
        Some(c) if c == ' ' || c == '\t' => (Some(checked), &after[c.len_utf8()..]),
        // Any other following character means this was never a checkbox.
        _ => (None, text),
    }
}

// ---- tables -----------------------------------------------------------------

fn scan_table_delimiter<F>(line: &str, mut push_align: F) -> Option<usize>
where
    F: FnMut(Align),
{
    let t = trim_space_tab(line);
    if !t.as_bytes().contains(&b'-') {
        return None;
    }
    let mut count = 0usize;
    for cell in table_delimiter_row_inner_from_trimmed(t).split('|') {
        let cell = trim_unicode_ws(cell);
        // Financial tables opt in explicitly: a terminal dot after at least
        // three hyphens requests a measured decimal anchor. Colon alignment
        // and ordinary GFM delimiters retain their existing interpretation.
        if let Some(core) = cell.strip_suffix('.')
            && core.len() >= 3
            && core.bytes().all(|byte| byte == b'-')
        {
            push_align(Align::Decimal);
            count += 1;
            continue;
        }
        if !cell.is_empty() {
            let core = cell.trim_start_matches(':').trim_end_matches(':');
            if core.is_empty() || !core.as_bytes().iter().all(|byte| *byte == b'-') {
                return None;
            }
        }
        let left = cell.starts_with(':');
        let right = cell.ends_with(':');
        push_align(match (left, right) {
            (true, true) => Align::Center,
            (true, false) => Align::Left,
            (false, true) => Align::Right,
            (false, false) => Align::None,
        });
        count += 1;
    }
    Some(count)
}

fn validated_table_delimiter_cell_count(line: &str) -> Option<usize> {
    scan_table_delimiter(line, |_| {})
}

fn validated_table_delimiter_alignments(line: &str) -> Option<Vec<Align>> {
    let mut align = Vec::new();
    scan_table_delimiter(line, |cell_align| align.push(cell_align))?;
    Some(align)
}

fn split_table_row_cell_count(line: &str) -> usize {
    let mut count = 0;
    visit_table_row_cells(line, |_| count += 1);
    count
}

fn split_table_row_into<'a>(line: &'a str, cells: &mut Vec<&'a str>) {
    cells.clear();
    visit_table_row_cells(line, |cell| cells.push(cell));
}

fn visit_table_row_cells<'a>(line: &'a str, mut emit: impl FnMut(&'a str)) {
    let t = trim_space_tab(line);
    let t = t.strip_prefix('|').unwrap_or(t);
    if !t.as_bytes().iter().any(|b| matches!(b, b'`' | b'\\')) {
        for cell in t.strip_suffix('|').unwrap_or(t).split('|') {
            emit(trim_unicode_ws(cell));
        }
        return;
    }
    // Preserve the established extension that protects unescaped pipes inside
    // matched code spans. An unmatched opener is literal and cannot hide later
    // cell separators. Index maximal backtick runs once, so adversarial rows
    // with many unmatched lengths cannot repeatedly rescan the remaining row.
    let bytes = t.as_bytes();
    let mut code_runs: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut pos = 0;
    while pos < bytes.len() {
        if bytes[pos] == b'`' {
            let len = ascii_run_len(bytes, pos, b'`');
            code_runs.entry(len).or_default().push(pos);
            pos += len;
        } else {
            pos += 1;
        }
    }
    let mut cell_start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            // Escapes apply outside code. Inside code the next same-length
            // run closes even when immediately preceded by a backslash.
            i += usize::from(i + 1 < bytes.len()) + 1;
        } else if bytes[i] == b'`' {
            let len = ascii_run_len(bytes, i, b'`');
            let close = code_runs
                .get(&len)
                .and_then(|runs| runs.get(runs.partition_point(|&start| start < i + len)));
            if let Some(&end) = close {
                i = end + len;
            } else {
                i += len;
            }
        } else if bytes[i] == b'|' {
            emit(trim_unicode_ws(&t[cell_start..i]));
            cell_start = i + 1;
            i += 1;
        } else {
            i += 1;
        }
    }
    // Only an actual final separator is optional. A final escaped pipe belongs
    // to the cell and must not be stripped before the scanner sees its escape.
    if cell_start < t.len() || t.is_empty() {
        emit(trim_unicode_ws(&t[cell_start..]));
    }
}

fn table_delimiter_row_inner_from_trimmed(t: &str) -> &str {
    let t = t.strip_prefix('|').unwrap_or(t);
    t.strip_suffix('|').unwrap_or(t)
}

fn ascii_run_len(bytes: &[u8], i: usize, byte: u8) -> usize {
    bytes[i..]
        .iter()
        .take_while(|&&candidate| candidate == byte)
        .count()
}

fn parse_table_profiled(
    lines: &[&str],
    refs: &ReferenceMap,
    profiler: &mut ParseProfiler,
    align: Vec<Align>,
) -> Option<(Table, TableExtent)> {
    let mut header = Vec::new();
    split_table_row_into(lines[0], &mut header);
    let cols = header.len();
    if cols == 0 || align.len() != cols {
        return None;
    }
    let head: Vec<Vec<Inline>> = header
        .iter()
        .map(|c| parse_table_cell_profiled(c, refs, profiler))
        .collect();
    let mut rows = Vec::new();
    let mut row_cells = Vec::with_capacity(cols);
    let mut body = TableBodyState::new(cols);
    let mut i = 2;
    let mut padding_exhausted = false;
    while i < lines.len() && is_table_row_source(lines[i]) {
        split_table_row_into(lines[i], &mut row_cells);
        if !body.admit_columns(row_cells.len()) {
            padding_exhausted = true;
            break;
        }
        let mut cells: Vec<Vec<Inline>> = row_cells
            .iter()
            .take(cols)
            .map(|c| parse_table_cell_profiled(c, refs, profiler))
            .collect();
        cells.resize_with(cols, Vec::new);
        rows.push(cells);
        i += 1;
    }
    Some((
        Table { align, head, rows },
        TableExtent {
            lines: i,
            padding_exhausted,
        },
    ))
}

fn parse_table_cell_profiled(
    text: &str,
    refs: &ReferenceMap,
    profiler: &mut ParseProfiler,
) -> Vec<Inline> {
    let previous = profiler.table_cell_code_escapes;
    profiler.table_cell_code_escapes = text.contains("\\|");
    let parsed = parse_inlines_with_refs_profiled(text, refs, profiler);
    profiler.table_cell_code_escapes = previous;
    parsed
}

// ---- inline parser ----------------------------------------------------------

/// Parse a run of text (which may contain `\n`) into inline elements.
#[must_use]
pub fn parse_inlines(text: &str) -> Vec<Inline> {
    parse_inlines_with_refs(text, &ReferenceMap::new())
}

fn parse_inlines_with_refs(text: &str, refs: &ReferenceMap) -> Vec<Inline> {
    let mut profiler = ParseProfiler::disabled();
    parse_inlines_with_refs_profiled(text, refs, &mut profiler)
}

fn parse_inlines_with_refs_profiled(
    text: &str,
    refs: &ReferenceMap,
    profiler: &mut ParseProfiler,
) -> Vec<Inline> {
    let started = profiler.checkpoint();
    let maybe_cacheable = profiler.destinations.is_none()
        && profiler.inline_parse_depth == 0
        && !profiler.table_cell_code_escapes
        && inline_cache_size_allows(text);
    let needs_full_parse = maybe_cacheable.then(|| inline_text_needs_full_parse(text));
    let cacheable = maybe_cacheable && needs_full_parse == Some(true);
    if cacheable && let Some(inlines) = profiler.inline_cache.get(text) {
        let char_count = if profiler.enabled {
            text.chars().count()
        } else {
            0
        };
        let allocations = if profiler.enabled {
            inline_tree_node_count(&inlines)
        } else {
            0
        };
        profiler.record_since(
            "inline_parse",
            char_count,
            text.len(),
            allocations,
            INLINE_PARSE_NOTES,
            started,
        );
        return inlines;
    }

    profiler.inline_parse_depth += 1;
    let inlines =
        parse_inlines_with_refs_profiled_uncached(text, refs, profiler, started, needs_full_parse);
    profiler.inline_parse_depth -= 1;

    if cacheable && profiler.inline_cache.admit(text) {
        profiler.inline_cache.insert(text, &inlines);
    }

    inlines
}

fn parse_inlines_with_refs_profiled_uncached(
    text: &str,
    refs: &ReferenceMap,
    profiler: &mut ParseProfiler,
    started: Option<ParseStageStart>,
    needs_full_parse: Option<bool>,
) -> Vec<Inline> {
    if !needs_full_parse.unwrap_or_else(|| inline_text_needs_full_parse(text)) {
        return record_plain_inline_parse(text, profiler, started);
    }
    let bytes = collect_inline_chars_from_text(text);
    if let Some(tracker) = &mut profiler.destinations {
        tracker.push_chars_text(&bytes, text);
    }
    let parsed = parse_inlines_chars_with_refs_profiled(
        &bytes,
        text.len(),
        refs,
        InlineContext::Prose,
        profiler,
        started,
    );
    if let Some(tracker) = &mut profiler.destinations {
        tracker.pop_chars();
    }
    parsed
}

fn record_plain_inline_parse(
    text: &str,
    profiler: &mut ParseProfiler,
    started: Option<ParseStageStart>,
) -> Vec<Inline> {
    let inlines = if text.is_empty() {
        Vec::new()
    } else {
        {
            let output = text.to_string();
            provenance::register(&output, provenance::text_origins(text, profiler));
            vec![Inline::Text(output)]
        }
    };
    let char_count = if profiler.enabled {
        text.chars().count()
    } else {
        0
    };
    profiler.record_since(
        "inline_parse",
        char_count,
        text.len(),
        inlines.len(),
        INLINE_PARSE_NOTES,
        started,
    );
    inlines
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InlineContext {
    Prose,
    /// An explicit link or image already owns this label. GFM's bare URL/email
    /// extension must leave its text alone; explicit nested links and angle
    /// autolinks still participate in CommonMark's inner-link precedence.
    LinkLabel,
}

/// Nested-inline entry for spans that already exist as a slice of the parent
/// run's char buffer (link text and reference-link text).
/// Behaves exactly like `parse_inlines_with_refs_profiled` at depth > 0 — no
/// cache consult or insert (admission is depth-0 only) — while skipping the
/// collect-to-String + re-decode round-trip through
/// `collect_inline_chars_from_text`.
fn parse_inlines_chars_nested(
    chars: &[char],
    refs: &ReferenceMap,
    context: InlineContext,
    profiler: &mut ParseProfiler,
) -> Vec<Inline> {
    if profiler.inline_work.exhausted
        || profiler.inline_work.depth >= MAX_INLINE_PARSE_RECURSION
        || !profiler.inline_work.charge(chars.len())
    {
        // Abandon speculative work without collecting another potentially large
        // label. The outermost run preserves the complete source as literal text
        // and rolls back any destination spans from its discarded parse.
        profiler.inline_work.exhausted = true;
        return Vec::new();
    }
    let started = profiler.checkpoint();
    profiler.inline_parse_depth += 1;
    let inlines = if !inline_chars_needs_full_parse(chars) {
        record_plain_inline_chars_parse(chars, profiler, started)
    } else {
        let byte_len = inline_chars_byte_len(chars, profiler.enabled);
        parse_inlines_chars_with_refs_profiled(chars, byte_len, refs, context, profiler, started)
    };
    profiler.inline_parse_depth -= 1;
    inlines
}

fn record_plain_inline_chars_parse(
    chars: &[char],
    profiler: &mut ParseProfiler,
    started: Option<ParseStageStart>,
) -> Vec<Inline> {
    let inlines = if chars.is_empty() {
        Vec::new()
    } else {
        {
            let output: String = chars.iter().collect();
            provenance::register(&output, provenance::char_origins(&output, chars, profiler));
            vec![Inline::Text(output)]
        }
    };
    let char_count = if profiler.enabled { chars.len() } else { 0 };
    profiler.record_since(
        "inline_parse",
        char_count,
        inline_chars_byte_len(chars, profiler.enabled),
        inlines.len(),
        INLINE_PARSE_NOTES,
        started,
    );
    inlines
}

/// Byte length of a char slice — the `str::len` of the equivalent text —
/// computed only when the profiler needs it for its stage ledger.
fn inline_chars_byte_len(chars: &[char], enabled: bool) -> usize {
    if enabled {
        chars.iter().map(|c| c.len_utf8()).sum()
    } else {
        0
    }
}

/// Parse a GFM footnote-reference id starting at `start` (just past `[^`).
/// Returns (id, index-past-the-closing-`]`) when the id is a GFM-legal id
/// (non-empty, no whitespace/`[`/`]`, no ASCII controls, ≤128 bytes).
fn parse_footnote_ref_id(bytes: &[char], start: usize) -> Option<(String, usize)> {
    let mut id = String::new();
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if c == ']' {
            if !is_safe_footnote_id(&id) {
                return None;
            }
            return Some((id, i + 1));
        }
        if !is_safe_footnote_id_char(c) {
            return None;
        }
        if id.len() >= 128 {
            return None;
        }
        id.push(c);
        i += 1;
    }
    None
}

fn inline_text_needs_full_parse(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' | b'\n' | b'\r' | b'`' | b'!' | b'[' | b'<' | b'&' | b'~' | b'*' | b'_'
            | b'@' | b'$' => return true,
            b':' if bytes[i..].starts_with(b"://") && inline_http_scheme_before_colon(bytes, i) => {
                return true;
            }
            b'.' if inline_www_prefix_before_dot(bytes, i) => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

fn inline_http_scheme_before_colon(bytes: &[u8], colon: usize) -> bool {
    (colon >= 4
        && bytes
            .get(colon - 4..colon)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case(b"http")))
        || (colon >= 5
            && bytes
                .get(colon - 5..colon)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case(b"https")))
}

fn inline_www_prefix_before_dot(bytes: &[u8], dot: usize) -> bool {
    dot >= 3
        && bytes
            .get(dot - 3..dot)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"www"))
}

/// Char-slice mirror of `inline_text_needs_full_parse` so nested inline spans
/// can be gated without materializing a String. Equivalent by construction:
/// every special byte is ASCII (UTF-8 never embeds ASCII bytes inside multibyte
/// sequences, so byte equality and char equality agree), and the `://`/`www`
/// checks are pure ASCII window comparisons at the same positions.
fn inline_chars_needs_full_parse(chars: &[char]) -> bool {
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if matches!(
            c,
            '\\' | '\n' | '\r' | '`' | '!' | '[' | '<' | '&' | '~' | '*' | '_' | '@' | '$'
        ) {
            return true;
        }
        if c == ':'
            && chars.get(i + 1) == Some(&'/')
            && chars.get(i + 2) == Some(&'/')
            && inline_chars_http_scheme_before_colon(chars, i)
        {
            return true;
        }
        if c == '.' && inline_chars_www_prefix_before_dot(chars, i) {
            return true;
        }
        i += 1;
    }
    false
}

fn inline_chars_http_scheme_before_colon(chars: &[char], colon: usize) -> bool {
    (colon >= 4 && starts_with_ascii_chars_ignore_case(chars, colon - 4, b"http"))
        || (colon >= 5 && starts_with_ascii_chars_ignore_case(chars, colon - 5, b"https"))
}

fn inline_chars_www_prefix_before_dot(chars: &[char], dot: usize) -> bool {
    dot >= 3 && starts_with_ascii_chars_ignore_case(chars, dot - 3, b"www")
}

fn parse_inlines_chars_with_refs_profiled(
    bytes: &[char],
    byte_len: usize,
    refs: &ReferenceMap,
    context: InlineContext,
    profiler: &mut ParseProfiler,
    started: Option<ParseStageStart>,
) -> Vec<Inline> {
    let outermost = profiler.inline_work.depth == 0;
    if outermost {
        profiler.inline_work.remaining = bytes
            .len()
            .saturating_mul(INLINE_PARSE_WORK_MULTIPLIER)
            .max(4096);
        profiler.inline_work.exhausted = false;
    }
    let destination_mark = profiler.destination_mark();
    profiler.inline_work.depth += 1;
    // Inline parsing is two phases. Phase 1 (this loop) tokenizes the text into a
    // flat list of `InlineEl` nodes: finalized inlines (code, links, images,
    // autolinks, raw HTML, breaks) interleaved with raw `*`/`_`/`~~`
    // delimiter runs. Phase 2 (`resolve_emphasis`) runs the CommonMark
    // "process emphasis" delimiter-stack algorithm over that list to pair openers
    // with closers and build the correct nested `Emphasis`/`Strong` tree.
    let mut els: Vec<InlineEl> = Vec::new();
    // A link label has fixed tokenization rules, so its candidates can share a
    // compact bracket table. Prose must instead probe only an actual tokenizer
    // candidate: a `[` inside an already consumed GFM bare URL is not an opener.
    let mut bracket_pairs: Option<BracketPairs> = None;
    let maybe_bare_email = context == InlineContext::Prose && bytes.contains(&'@');
    let mut buf = provenance::Buffer::default();
    let mut i = 0;
    let mut has_emphasis_delimiters = false;
    let flush = |buf: &mut provenance::Buffer, els: &mut Vec<InlineEl>| {
        if !buf.is_empty() {
            els.push(InlineEl::Text(std::mem::take(buf).finish()));
        }
    };
    while i < bytes.len() {
        if profiler.inline_work.exhausted {
            break;
        }
        let c = bytes[i];
        let previous = buf.len();
        let consumed_start = i;
        match c {
            '\\' if i + 1 < bytes.len() && is_ascii_punct(bytes[i + 1]) => {
                buf.push(bytes[i + 1]);
                i += 2;
            }
            '\n' => {
                // Only source delimiters form a hard break. The decoded buffer
                // can end in entity-produced spaces/backslashes or an escaped
                // literal backslash; those remain content, not syntax.
                let trailing_spaces = bytes[..i].iter().rev().take_while(|&&ch| ch == ' ').count();
                let hard_backslash = bytes[..i]
                    .iter()
                    .rev()
                    .take_while(|&&ch| ch == '\\')
                    .count()
                    % 2
                    == 1;
                let hard = trailing_spaces >= 2 || hard_backslash;
                for _ in 0..trailing_spaces {
                    buf.pop();
                }
                if hard_backslash {
                    buf.pop();
                }
                flush(&mut buf, &mut els);
                els.push(InlineEl::Node(if hard {
                    Inline::HardBreak
                } else {
                    provenance::soft_break(bytes, i, profiler)
                }));
                i += 1;
                while i < bytes.len() && (bytes[i] == ' ' || bytes[i] == '\t') {
                    i += 1;
                }
            }
            '`' => {
                let Some(n) = run_len(bytes, i, '`', &mut profiler.inline_work) else {
                    break;
                };
                if let Some(end) = find_code_close(bytes, i + n, '`', n, &mut profiler.inline_work)
                {
                    flush(&mut buf, &mut els);
                    let code = normalize_code_span(&bytes[i + n..end]);
                    let code = if profiler.table_cell_code_escapes && code.contains("\\|") {
                        code.replace("\\|", "|")
                    } else {
                        code
                    };
                    provenance::register(&code, code_origins(&code, &bytes[i + n..end], profiler));
                    els.push(InlineEl::Node(Inline::Code(code)));
                    i = end + n;
                } else {
                    for _ in 0..n {
                        buf.push('`');
                    }
                    i += n;
                }
            }
            '!' if i + 1 < bytes.len() && bytes[i + 1] == '[' => {
                let close = if context == InlineContext::LinkLabel {
                    bracket_pairs
                        .get_or_insert_with(|| {
                            compute_bracket_pairs(bytes, refs, &mut profiler.inline_work)
                        })
                        .partner(i + 1)
                } else {
                    find_link_label_close(bytes, i, i + 1, refs, &mut profiler.inline_work)
                };
                if profiler.inline_work.exhausted {
                    break;
                }
                if let Some((alt, dest, title, next, token)) =
                    parse_link_like(bytes, i + 1, close, refs, profiler, true)
                {
                    if let Some(tracker) = &mut profiler.destinations {
                        tracker.record(bytes, token, &dest);
                    }
                    flush(&mut buf, &mut els);
                    els.push(InlineEl::Node(Inline::Image {
                        dest,
                        title,
                        alt: inlines_to_plain(&alt),
                    }));
                    i = next;
                } else if let Some((alt, dest, title, next)) =
                    parse_reference_link_like(bytes, i + 1, close, refs, profiler, true)
                {
                    flush(&mut buf, &mut els);
                    els.push(InlineEl::Node(Inline::Image {
                        dest,
                        title,
                        alt: inlines_to_plain(&alt),
                    }));
                    i = next;
                } else {
                    buf.push(c);
                    i += 1;
                }
            }
            '[' if bytes.get(i + 1) == Some(&'^') => {
                // GFM footnote reference: [^id] where id has no spaces or
                // brackets. Emits FootnoteRef; the renderer numbers it and
                // drops refs whose id has no definition (post-parse pass).
                match parse_footnote_ref_id(bytes, i + 2) {
                    Some((id, next)) => {
                        flush(&mut buf, &mut els);
                        provenance::register(
                            &id,
                            provenance::char_origins(&id, &bytes[i + 2..next - 1], profiler),
                        );
                        els.push(InlineEl::Node(Inline::FootnoteRef { id }));
                        i = next;
                    }
                    None => {
                        buf.push(c);
                        i += 1;
                    }
                }
            }
            '[' => {
                // CommonMark forbids links inside links at any nesting level (an
                // inner link wins; the outer brackets stay literal). Images are
                // exempt — their description is flattened to alt text — so this
                // guard applies only to the link-forming paths, not the `!` image
                // path above. Without it, `[a [b](/u)](/u)` emits nested <a>.
                let close = if context == InlineContext::LinkLabel {
                    bracket_pairs
                        .get_or_insert_with(|| {
                            compute_bracket_pairs(bytes, refs, &mut profiler.inline_work)
                        })
                        .partner(i)
                } else {
                    find_link_label_close(bytes, i, i, refs, &mut profiler.inline_work)
                };
                if profiler.inline_work.exhausted {
                    break;
                }
                if let Some((content, dest, title, next, token)) =
                    parse_link_like(bytes, i, close, refs, profiler, false)
                {
                    if let Some(tracker) = &mut profiler.destinations {
                        tracker.record(bytes, token, &dest);
                    }
                    flush(&mut buf, &mut els);
                    els.push(InlineEl::Node(Inline::Link {
                        dest,
                        title,
                        content,
                    }));
                    i = next;
                } else if let Some((content, dest, title, next)) =
                    parse_reference_link_like(bytes, i, close, refs, profiler, false)
                {
                    flush(&mut buf, &mut els);
                    els.push(InlineEl::Node(Inline::Link {
                        dest,
                        title,
                        content,
                    }));
                    i = next;
                } else if let Some((html, next)) =
                    parse_inline_html(bytes, i, &mut profiler.inline_work)
                {
                    if let Some(tracker) = &mut profiler.destinations {
                        tracker.inline_html(bytes, i, &html);
                    }
                    flush(&mut buf, &mut els);
                    provenance::register(
                        &html,
                        provenance::char_origins(&html, &bytes[i..next], profiler),
                    );
                    els.push(InlineEl::Node(Inline::Html(html)));
                    i = next;
                } else {
                    buf.push(c);
                    i += 1;
                }
            }
            '<' => {
                if let Some((label, dest, next)) =
                    parse_autolink(bytes, i, &mut profiler.inline_work)
                {
                    flush(&mut buf, &mut els);
                    els.push(InlineEl::Node(Inline::Link {
                        dest,
                        title: None,
                        content: {
                            let start = if bytes[i] == '<' { i + 1 } else { i };
                            provenance::register(
                                &label,
                                provenance::char_origins(
                                    &label,
                                    &bytes[start..start + label.chars().count()],
                                    profiler,
                                ),
                            );
                            vec![Inline::Text(label)]
                        },
                    }));
                    i = next;
                } else if let Some((html, next)) =
                    parse_inline_html(bytes, i, &mut profiler.inline_work)
                {
                    if let Some(tracker) = &mut profiler.destinations {
                        tracker.inline_html(bytes, i, &html);
                    }
                    flush(&mut buf, &mut els);
                    provenance::register(
                        &html,
                        provenance::char_origins(&html, &bytes[i..next], profiler),
                    );
                    els.push(InlineEl::Node(Inline::Html(html)));
                    i = next;
                } else {
                    buf.push(c);
                    i += 1;
                }
            }
            '&' => {
                if let Some((decoded, next)) = parse_character_reference(bytes, i) {
                    buf.push_str(&decoded);
                    i = next;
                } else {
                    buf.push(c);
                    i += 1;
                }
            }
            '$' => {
                let Some(n) = run_len(bytes, i, '$', &mut profiler.inline_work) else {
                    break;
                };
                if n >= 2
                    && let Some((inner, next)) =
                        parse_math_delim(bytes, i, 2, &mut profiler.inline_work)
                {
                    flush(&mut buf, &mut els);
                    provenance::register(
                        &inner,
                        provenance::char_origins(&inner, &bytes[i + 2..next - 2], profiler),
                    );
                    els.push(InlineEl::Node(Inline::DisplayMath(inner)));
                    i = next;
                    continue;
                }
                if n == 1
                    && let Some((inner, next)) =
                        parse_math_delim(bytes, i, 1, &mut profiler.inline_work)
                {
                    flush(&mut buf, &mut els);
                    provenance::register(
                        &inner,
                        provenance::char_origins(&inner, &bytes[i + 1..next - 1], profiler),
                    );
                    els.push(InlineEl::Node(Inline::Math(inner)));
                    i = next;
                    continue;
                }
                buf.push(c);
                i += 1;
            }
            '*' | '_' | '~' => {
                // Push a delimiter run onto the node list with its
                // source-boundary can_open/can_close flags. The actual pairing
                // into emphasis/strong/strike is deferred to
                // `resolve_emphasis` (the delimiter-stack pass) so that nested and
                // overlapping runs resolve correctly.
                let Some(n) = run_len(bytes, i, c, &mut profiler.inline_work) else {
                    break;
                };
                let before = i.checked_sub(1).map(|idx| bytes[idx]);
                let after = bytes.get(i + n).copied();
                // Strike keeps its source-whitespace boundary rule. Punctuation,
                // including an entity's final `;`, must not prevent a valid pair
                // next to a word. Emphasis retains CommonMark flanking rules.
                let (can_open, can_close) = if c == '~' {
                    (
                        after.is_some_and(|ch| !ch.is_whitespace()),
                        before.is_some_and(|ch| !ch.is_whitespace()),
                    )
                } else {
                    emphasis_flanking(before, after, c)
                };
                // This renderer supports double-tilde strikethrough. A longer
                // run is literal, not an opener embedded in a partial run.
                let supported_run = c != '~' || n == 2;
                if supported_run && (can_open || can_close) {
                    flush(&mut buf, &mut els);
                    els.push(InlineEl::Delim {
                        ch: c,
                        count: n,
                        orig: n,
                        origins: provenance::origins_for_chars(&bytes[i..i + n], profiler),
                        can_open,
                        can_close,
                    });
                    has_emphasis_delimiters = true;
                } else {
                    // An inert run (e.g. an intraword `_`) is literal text.
                    for _ in 0..n {
                        buf.push(c);
                    }
                }
                i += n;
            }
            _ => {
                let mut bare_autolink = None;
                if context == InlineContext::Prose && inline_chars_maybe_bare_url_start(bytes, i) {
                    bare_autolink = parse_bare_url_autolink(bytes, i, &mut profiler.inline_work);
                }
                if bare_autolink.is_none()
                    && maybe_bare_email
                    && inline_char_maybe_bare_email_start(c)
                {
                    bare_autolink = parse_bare_email_autolink(bytes, i, &mut profiler.inline_work);
                }
                if let Some((label, dest, next)) = bare_autolink {
                    flush(&mut buf, &mut els);
                    els.push(InlineEl::Node(Inline::Link {
                        dest,
                        title: None,
                        content: {
                            let start = if bytes[i] == '<' { i + 1 } else { i };
                            provenance::register(
                                &label,
                                provenance::char_origins(
                                    &label,
                                    &bytes[start..start + label.chars().count()],
                                    profiler,
                                ),
                            );
                            vec![Inline::Text(label)]
                        },
                    }));
                    i = next;
                } else {
                    buf.push(c);
                    i += 1;
                }
            }
        }
        buf.record(previous, bytes, consumed_start..i, profiler);
    }
    profiler.inline_work.depth -= 1;
    if profiler.inline_work.exhausted {
        if outermost {
            profiler.destination_rollback(destination_mark);
            return record_plain_inline_chars_parse(bytes, profiler, started);
        }
        return Vec::new();
    }
    // Trim only trailing source spaces, preserving decoded character references
    // at paragraph ends and inside nested link content.
    let trailing_spaces = bytes.iter().rev().take_while(|&&ch| ch == ' ').count();
    for _ in 0..trailing_spaces {
        buf.pop();
    }
    flush(&mut buf, &mut els);
    let resolver_allocations = if has_emphasis_delimiters {
        bytes.len()
    } else {
        0
    };
    let out = if has_emphasis_delimiters {
        resolve_emphasis(els)
    } else {
        finish_inline_elements_without_delimiters(els)
    };
    profiler.record_since(
        "inline_parse",
        bytes.len(),
        byte_len,
        2 + resolver_allocations + out.len(),
        INLINE_PARSE_NOTES,
        started,
    );
    out
}

fn inline_tree_node_count(inlines: &[Inline]) -> usize {
    inlines
        .iter()
        .map(|inline| match inline {
            Inline::Emphasis(content)
            | Inline::Strong(content)
            | Inline::Strikethrough(content) => 1 + inline_tree_node_count(content),
            Inline::Link { content, .. } => 1 + inline_tree_node_count(content),
            Inline::Text(_)
            | Inline::Code(_)
            | Inline::Math(_)
            | Inline::DisplayMath(_)
            | Inline::Image { .. }
            | Inline::FootnoteRef { .. }
            | Inline::SoftBreak
            | Inline::HardBreak
            | Inline::Html(_) => 1,
        })
        .sum()
}

#[inline(always)]
fn inline_chars_maybe_bare_url_start(chars: &[char], idx: usize) -> bool {
    match chars.get(idx) {
        Some('h' | 'H') => {
            starts_with_ascii_chars_ignore_case(chars, idx, b"http://")
                || starts_with_ascii_chars_ignore_case(chars, idx, b"https://")
        }
        Some('w' | 'W') => starts_with_ascii_chars_ignore_case(chars, idx, b"www."),
        _ => false,
    }
}

#[inline(always)]
const fn inline_char_maybe_bare_email_start(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '+')
}

/// One element of the flat inline token list built before emphasis resolution.
enum InlineEl {
    /// A run of plain text (entities already decoded, escapes already applied).
    Text(String),
    /// A finalized inline that emphasis processing treats as opaque (code,
    /// links, images, autolinks, raw HTML, breaks, and emphasis/strong/strike
    /// subtrees produced by `resolve_emphasis` itself).
    Node(Inline),
    /// An unresolved run of `*`, `_`, or `~~` delimiters.
    Delim {
        ch: char,
        /// Remaining (not yet consumed) delimiter characters.
        count: usize,
        /// Original run length, used for the CommonMark "rule of three".
        orig: usize,
        origins: Vec<provenance::Origin>,
        can_open: bool,
        can_close: bool,
    },
}

/// CommonMark left/right-flanking classification for an emphasis run.
///
/// Returns `(can_open, can_close)`. `before`/`after` are the characters
/// immediately adjacent to the run (the start and end of the text count as
/// whitespace). For `_`, the additional intraword rule applies so that
/// `foo_bar` stays literal while `_foo_` opens/closes emphasis.
fn emphasis_flanking(before: Option<char>, after: Option<char>, ch: char) -> (bool, bool) {
    // CommonMark 0.31.2 counts an ASCII punctuation character OR any Unicode P
    // (punctuation) or S (symbol) code point as "punctuation" for flanking, so a
    // symbol like `£`/`€` adjacent to a delimiter suppresses emphasis just like
    // ASCII punctuation does.
    let before_ws = before.is_none_or(char::is_whitespace);
    let before_punct = before.is_some_and(unicode_punct::is_unicode_punctuation);
    let after_ws = after.is_none_or(char::is_whitespace);
    let after_punct = after.is_some_and(unicode_punct::is_unicode_punctuation);

    let left_flanking = !after_ws && (!after_punct || before_ws || before_punct);
    let right_flanking = !before_ws && (!before_punct || after_ws || after_punct);

    if ch == '_' {
        let can_open = left_flanking && (!right_flanking || before_punct);
        let can_close = right_flanking && (!left_flanking || after_punct);
        (can_open, can_close)
    } else {
        (left_flanking, right_flanking)
    }
}

/// Append text to `out`, merging with a trailing `Text` node when possible so the
/// resolved inline sequence keeps adjacent literal runs coalesced.
fn push_inline_text(out: &mut Vec<Inline>, s: &str) {
    if s.is_empty() {
        return;
    }
    if let Some(Inline::Text(t)) = out.last_mut() {
        provenance::append(t, s);
    } else {
        out.push(Inline::Text(provenance::copy(s)));
    }
}

/// Move an element's content into `out` without cloning. Unmatched delimiter
/// runs degrade to their literal characters.
fn emit_inline_el_owned(el: InlineEl, out: &mut Vec<Inline>) {
    match el {
        InlineEl::Text(s) => {
            if s.is_empty() {
                return;
            }
            if let Some(Inline::Text(t)) = out.last_mut() {
                provenance::append(t, &s);
            } else {
                out.push(Inline::Text(s));
            }
        }
        InlineEl::Node(inl) => out.push(inl),
        InlineEl::Delim {
            ch, count, origins, ..
        } => {
            let mut s = String::with_capacity(count);
            for _ in 0..count {
                s.push(ch);
            }
            provenance::register(&s, origins);
            push_inline_text(out, &s);
        }
    }
}

fn finish_inline_elements_without_delimiters(els: Vec<InlineEl>) -> Vec<Inline> {
    let mut out = Vec::with_capacity(els.len());
    for el in els {
        emit_inline_el_owned(el, &mut out);
    }
    out
}

#[derive(Clone, Copy)]
struct EmphasisNode {
    prev: Option<usize>,
    next: Option<usize>,
    node_depth: usize,
    alive: bool,
}

struct EmphasisArena {
    nodes: Vec<EmphasisNode>,
}

impl EmphasisArena {
    fn new(n: usize) -> Self {
        let mut nodes = Vec::with_capacity(n.saturating_mul(2));
        for i in 0..n {
            nodes.push(EmphasisNode {
                prev: i.checked_sub(1),
                next: if i + 1 < n { Some(i + 1) } else { None },
                node_depth: 1,
                alive: true,
            });
        }
        Self { nodes }
    }

    #[inline(always)]
    fn unlink(&mut self, x: usize, head: &mut Option<usize>) {
        let p = self.nodes[x].prev;
        let nx = self.nodes[x].next;
        match p {
            Some(pp) => self.nodes[pp].next = nx,
            None => *head = nx,
        }
        if let Some(nn) = nx {
            self.nodes[nn].prev = p;
        }
        self.nodes[x].alive = false;
    }
}

/// Resolve a flat token list into a nested inline tree using the CommonMark
/// "process emphasis" delimiter-stack algorithm, then linearize what remains.
fn resolve_emphasis(els: Vec<InlineEl>) -> Vec<Inline> {
    let n = els.len();
    let mut els = els;
    let mut arena = EmphasisArena::new(n);
    let mut head: Option<usize> = (n > 0).then_some(0);

    process_emphasis(&mut els, &mut arena, &mut head);

    let mut out = Vec::new();
    let mut idx = head;
    while let Some(k) = idx {
        let next_idx = arena.nodes[k].next;
        if arena.nodes[k].alive {
            let taken = std::mem::replace(&mut els[k], InlineEl::Text(String::new()));
            emit_inline_el_owned(taken, &mut out);
        }
        idx = next_idx;
    }
    out
}

/// Defensive bound on emphasis/strong/strike nesting depth. Real prose never nests
/// inline emphasis more than a handful deep; a pathological run like
/// `***…***x***…***` would otherwise build a tree thousands of levels deep,
/// which overflows the stack when it is later rendered or dropped (recursive
/// descent over `Inline`). Past this cap we stop wrapping and leave the surplus
/// delimiters as literal text — mirroring [`MAX_BLOCK_NESTING_DEPTH`].
const MAX_INLINE_NESTING_DEPTH: usize = 1000;
const OPENERS_BOTTOM_SLOTS_PER_CHAR: usize = 6;
const OPENERS_BOTTOM_LEN: usize = OPENERS_BOTTOM_SLOTS_PER_CHAR * 3;

fn opener_bottom_index(ch: char, slot: usize) -> usize {
    debug_assert!(matches!(ch, '*' | '_' | '~'));
    debug_assert!(slot < OPENERS_BOTTOM_SLOTS_PER_CHAR);
    let character = match ch {
        '*' => 0,
        '_' => 1,
        _ => 2,
    };
    character * OPENERS_BOTTOM_SLOTS_PER_CHAR + slot
}

/// The CommonMark "process emphasis" pass: walk closers left to right, match each
/// to the nearest compatible opener honoring the rule of three, and wrap the
/// enclosed nodes in `Strong` (2 delimiters), `Emphasis` (1 delimiter), or
/// `Strikethrough` (2 tildes).
fn process_emphasis(els: &mut Vec<InlineEl>, arena: &mut EmphasisArena, head: &mut Option<usize>) {
    let mut openers_bottom = [None; OPENERS_BOTTOM_LEN];
    let step_budget = els.len().saturating_mul(64).max(4096);
    let mut steps: usize = 0;
    let mut ci = *head;

    while let Some(c) = ci {
        if !arena.nodes[c].alive {
            ci = arena.nodes[c].next;
            continue;
        }
        let (cch, closer_can_open, corig) = match &els[c] {
            InlineEl::Delim {
                ch,
                can_close,
                can_open,
                orig,
                ..
            } if *can_close => (*ch, *can_open, *orig),
            _ => {
                ci = arena.nodes[c].next;
                continue;
            }
        };
        let slot = (if closer_can_open { 3 } else { 0 }) + (corig % 3);
        let bottom_idx = opener_bottom_index(cch, slot);
        let bound = openers_bottom[bottom_idx];

        // Walk back to the nearest opener of the same char that is not rejected
        // by the rule of three.
        let mut opener_idx = arena.nodes[c].prev;
        let mut found: Option<usize> = None;
        while let Some(o) = opener_idx {
            steps += 1;
            if steps > step_budget {
                return;
            }
            if Some(o) == bound {
                break;
            }
            if arena.nodes[o].alive
                && let InlineEl::Delim {
                    ch,
                    can_open,
                    can_close,
                    orig,
                    ..
                } = &els[o]
                && *ch == cch
                && *can_open
            {
                // Rule of three: if either delimiter can both open and close, the
                // summed run lengths must not be a multiple of 3 unless both are.
                let odd_match = cch != '~'
                    && (closer_can_open || *can_close)
                    && (*orig + corig) % 3 == 0
                    && !(*orig % 3 == 0 && corig % 3 == 0);
                if !odd_match {
                    found = Some(o);
                    break;
                }
            }
            opener_idx = arena.nodes[o].prev;
        }

        let Some(o) = found else {
            openers_bottom[bottom_idx] = arena.nodes[c].prev;
            ci = arena.nodes[c].next;
            continue;
        };

        let ocount = match &els[o] {
            InlineEl::Delim { count, .. } => *count,
            _ => 0,
        };
        let ccount = match &els[c] {
            InlineEl::Delim { count, .. } => *count,
            _ => 0,
        };
        let use_delims = if ocount >= 2 && ccount >= 2 { 2 } else { 1 };

        let mut max_child_depth = 0usize;
        let mut m = arena.nodes[o].next;
        while let Some(mi) = m.filter(|&mi| mi != c) {
            max_child_depth = max_child_depth.max(arena.nodes[mi].node_depth);
            m = arena.nodes[mi].next;
        }
        if max_child_depth >= MAX_INLINE_NESTING_DEPTH {
            ci = arena.nodes[c].next;
            continue;
        }

        // Collect and consume the nodes strictly between opener and closer.
        let mut content: Vec<Inline> = Vec::new();
        let mut m = arena.nodes[o].next;
        while let Some(mi) = m {
            if mi == c {
                break;
            }
            let nxt = arena.nodes[mi].next;
            if arena.nodes[mi].alive {
                arena.nodes[mi].alive = false;
                let taken = std::mem::replace(&mut els[mi], InlineEl::Text(String::new()));
                emit_inline_el_owned(taken, &mut content);
            }
            m = nxt;
        }
        let node = if cch == '~' {
            Inline::Strikethrough(content)
        } else if use_delims == 2 {
            Inline::Strong(content)
        } else {
            Inline::Emphasis(content)
        };

        if let InlineEl::Delim { count, origins, .. } = &mut els[o] {
            *count -= use_delims;
            origins.retain(|origin| origin.bytes.start < *count);
        }
        if let InlineEl::Delim { count, origins, .. } = &mut els[c] {
            *count -= use_delims;
            origins.retain_mut(|origin| {
                if origin.bytes.end <= use_delims {
                    return false;
                }
                origin.bytes.start -= use_delims;
                origin.bytes.end -= use_delims;
                true
            });
        }

        // Splice the new node between the (possibly shortened) opener and closer.
        let ni = els.len();
        els.push(InlineEl::Node(node));
        arena.nodes.push(EmphasisNode {
            prev: Some(o),
            next: Some(c),
            node_depth: max_child_depth + 1,
            alive: true,
        });
        arena.nodes[o].next = Some(ni);
        arena.nodes[c].prev = Some(ni);

        if matches!(&els[o], InlineEl::Delim { count, .. } if *count == 0) {
            arena.unlink(o, head);
        }
        if matches!(&els[c], InlineEl::Delim { count, .. } if *count == 0) {
            let after = arena.nodes[c].next;
            arena.unlink(c, head);
            ci = after;
        } else {
            ci = Some(c);
        }
    }
}

#[inline(always)]
fn run_len(chars: &[char], i: usize, ch: char, work: &mut InlineWorkBudget) -> Option<usize> {
    let count = chars[i..].iter().take_while(|&&c| c == ch).count();
    work.charge(count).then_some(count)
}

#[inline(always)]
fn find_code_close(
    chars: &[char],
    from: usize,
    ch: char,
    n: usize,
    work: &mut InlineWorkBudget,
) -> Option<usize> {
    // A code span opened by `n` backticks closes only on a maximal run of
    // exactly `n`. Charge failed searches too: many distinct unmatched run
    // lengths would otherwise each rescan the entire remaining paragraph.
    let mut i = from;
    let mut found = None;
    while i < chars.len() {
        if chars[i] == ch {
            let run = run_len(chars, i, ch, work)?;
            if run == n {
                found = Some(i);
                break;
            }
            i += run;
        } else {
            i += 1;
        }
    }
    if !work.charge(i - from) {
        return None;
    }
    found
}

fn normalize_code_span(chars: &[char]) -> String {
    // CommonMark: collapse internal line endings to spaces; strip one leading and
    // trailing space if the span is not all spaces.
    //
    // Single-collect discipline: the span String is materialized exactly once,
    // straight from the char slice. The strip guard mirrors the old
    // post-`replace` test exactly: `replace` mapped each '\n' (1 UTF-8 byte) to
    // ' ' (1 byte), so a first/last '\n' is a first/last ' ' afterwards, the
    // byte-strip `s[1..len-1]` of the replaced string equals this char-strip,
    // and '\n' -> ' ' preserves every char's whitespace class, so
    // `s.trim() != ""` equals `any(!char::is_whitespace)`. Clean spans (no '\n',
    // no strip — the common shape) collect with zero extra passes; newline
    // spans fold '\n' -> ' ' during the collect, byte-identical to
    // `str::replace` because each newline is exactly one byte becoming exactly
    // one byte (runs are not collapsed: one space per newline, as before).
    let strip = chars.len() >= 2
        && matches!(chars[0], ' ' | '\n')
        && matches!(chars[chars.len() - 1], ' ' | '\n')
        && chars.iter().any(|c| !c.is_whitespace());
    let body = if strip {
        &chars[1..chars.len() - 1]
    } else {
        chars
    };
    if body.contains(&'\n') {
        body.iter()
            .map(|&c| if c == '\n' { ' ' } else { c })
            .collect()
    } else {
        body.iter().collect()
    }
}

/// Parse a math delimiter run `$` or `$$`, returning inner TeX text and next index.
fn parse_math_delim(
    chars: &[char],
    i: usize,
    want: usize,
    work: &mut InlineWorkBudget,
) -> Option<(String, usize)> {
    let open_run = run_len(chars, i, '$', work)?;
    if open_run != want {
        return None;
    }
    let after = i + want;
    if after >= chars.len() || chars[after] == ' ' || chars[after] == '\t' || chars[after] == '\n' {
        return None;
    }
    let mut j = after;
    let mut found = None;
    while j < chars.len() {
        if chars[j] == '\\' && j + 1 < chars.len() {
            j += 2;
            continue;
        }
        if chars[j] == '$' {
            let run = run_len(chars, j, '$', work)?;
            if run == want
                && j > after
                && chars[j - 1] != ' '
                && chars[j - 1] != '\t'
                && chars[j - 1] != '\n'
            {
                found = Some(j);
                break;
            }
            j += run;
        } else {
            j += 1;
        }
    }
    if !work.charge(j - after) {
        return None;
    }
    let end = found?;
    let inner: String = chars[after..end].iter().collect();
    Some((inner, end + want))
}

/// True when `inlines` contain a link at any nesting depth. CommonMark bans
/// links inside links, so a candidate link whose text contains one must not form
/// (the inner link wins). Recurses through emphasis/strong/strikethrough; images
/// carry only flattened alt text, so they can never hold an inline link.
fn contains_link(inlines: &[Inline]) -> bool {
    inlines.iter().any(|inl| match inl {
        Inline::Link { .. } => true,
        Inline::Emphasis(c) | Inline::Strong(c) | Inline::Strikethrough(c) => contains_link(c),
        _ => false,
    })
}

fn skip_link_whitespace(chars: &[char], i: &mut usize) {
    let mut saw_newline = false;
    while *i < chars.len() {
        match chars[*i] {
            ' ' | '\t' => {
                *i += 1;
            }
            '\n' => {
                if saw_newline {
                    break;
                }
                saw_newline = true;
                *i += 1;
            }
            _ => break,
        }
    }
}

/// A parsed inline link or image: content, destination, title, end index, source span.
type LinkLikeParse = (Vec<Inline>, String, Option<String>, usize, Range<usize>);

/// The destination/title suffix of an inline link or image. Both bracket
/// matching and AST construction use this grammar, so punctuation in an accepted
/// destination or title cannot become brackets, code, or mathematics in its label.
struct InlineLinkTarget {
    dest: String,
    title: Option<String>,
    next: usize,
    token: Range<usize>,
}

fn parse_inline_link_target(
    chars: &[char],
    close: usize,
    work: &mut InlineWorkBudget,
) -> Option<InlineLinkTarget> {
    if chars.get(close) != Some(&']') || chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let mut k = close + 2;
    let target = (|| {
        skip_link_whitespace(chars, &mut k);
        let destination_start = k;
        let mut destination_end = k;
        let (dest, title) = if chars.get(k) == Some(&')') {
            (String::new(), None)
        } else {
            let dest = parse_link_destination(chars, &mut k)?;
            destination_end = k;
            let spaces_start = k;
            skip_link_whitespace(chars, &mut k);
            let title = if chars.get(k) == Some(&')') {
                None
            } else {
                if k == spaces_start {
                    return None;
                }
                let title = parse_link_title(chars, &mut k)?;
                skip_link_whitespace(chars, &mut k);
                Some(title)
            };
            (dest, title)
        };
        if chars.get(k) != Some(&')') {
            return None;
        }
        Some(InlineLinkTarget {
            dest,
            title,
            next: k + 1,
            token: destination_start..destination_end,
        })
    })();
    // Include failed destination/title scans and all interstitial whitespace.
    // A rejected suffix remains ordinary source, but retrying it is never free.
    work.charge(k.saturating_sub(close).saturating_add(1))
        .then_some(target)
        .flatten()
}

/// Parse `[content](dest "title")` starting at the `[`.
fn parse_link_like(
    chars: &[char],
    i: usize,
    close: Option<usize>,
    refs: &ReferenceMap,
    profiler: &mut ParseProfiler,
    image: bool,
) -> Option<LinkLikeParse> {
    if chars.get(i) != Some(&'[') {
        return None;
    }
    let close = close?;
    let target = parse_inline_link_target(chars, close, &mut profiler.inline_work)?;
    let mark = profiler.destination_mark();
    let content = parse_inlines_chars_nested(
        &chars[i + 1..close],
        refs,
        InlineContext::LinkLabel,
        profiler,
    );
    let nested_link = !image && contains_link(&content);
    if nested_link {
        profiler.destination_rollback(mark);
    } else if image {
        profiler.destination_tokens_rollback(mark);
    }
    if nested_link {
        return None;
    }
    Some((
        content,
        target.dest,
        target.title,
        target.next,
        target.token,
    ))
}

fn parse_link_destination(chars: &[char], i: &mut usize) -> Option<String> {
    if chars.get(*i) == Some(&'<') {
        parse_angle_link_destination(chars, i)
    } else {
        parse_bare_link_destination(chars, i)
    }
}

fn parse_angle_link_destination(chars: &[char], i: &mut usize) -> Option<String> {
    if chars.get(*i) != Some(&'<') {
        return None;
    }
    *i += 1;
    let mut dest = String::new();
    while let Some(&ch) = chars.get(*i) {
        match ch {
            '>' => {
                *i += 1;
                return Some(dest);
            }
            '\n' | '<' => return None,
            '\\' if chars.get(*i + 1).is_some_and(|&next| is_ascii_punct(next)) => {
                dest.push(chars[*i + 1]);
                *i += 2;
            }
            '&' => {
                if let Some((decoded, next)) = parse_character_reference(chars, *i) {
                    dest.push_str(&decoded);
                    *i = next;
                } else {
                    dest.push(ch);
                    *i += 1;
                }
            }
            _ => {
                dest.push(ch);
                *i += 1;
            }
        }
    }
    None
}

fn parse_bare_link_destination(chars: &[char], i: &mut usize) -> Option<String> {
    let mut dest = String::new();
    let mut paren_depth = 0usize;

    while let Some(&ch) = chars.get(*i) {
        match ch {
            ')' if paren_depth == 0 => break,
            ')' => {
                paren_depth -= 1;
                dest.push(ch);
                *i += 1;
            }
            '(' => {
                paren_depth += 1;
                dest.push(ch);
                *i += 1;
            }
            '<' => return None,
            ch if ch == ' ' || ch == '\t' || ch == '\r' || ch == '\n' => break,
            ch if ch.is_ascii_control() => return None,
            '\\' if chars.get(*i + 1).is_some_and(|&next| is_ascii_punct(next)) => {
                dest.push(chars[*i + 1]);
                *i += 2;
            }
            '&' => {
                if let Some((decoded, next)) = parse_character_reference(chars, *i) {
                    dest.push_str(&decoded);
                    *i = next;
                } else {
                    dest.push(ch);
                    *i += 1;
                }
            }
            _ => {
                dest.push(ch);
                *i += 1;
            }
        }
    }

    if paren_depth == 0 && !dest.is_empty() {
        Some(dest)
    } else {
        None
    }
}

fn parse_link_title(chars: &[char], i: &mut usize) -> Option<String> {
    let (open, close) = match chars.get(*i).copied()? {
        '"' => ('"', '"'),
        '\'' => ('\'', '\''),
        '(' => ('(', ')'),
        _ => return None,
    };
    if chars.get(*i) != Some(&open) {
        return None;
    }
    *i += 1;

    let mut title = String::new();
    let mut saw_newline = false;
    while let Some(&ch) = chars.get(*i) {
        match ch {
            c if c == close => {
                *i += 1;
                return Some(title);
            }
            '(' if open == '(' => return None,
            '\n' => {
                if saw_newline {
                    return None;
                }
                saw_newline = true;
                title.push(ch);
                *i += 1;
            }
            '\\' if chars.get(*i + 1).is_some_and(|&next| is_ascii_punct(next)) => {
                saw_newline = false;
                title.push(chars[*i + 1]);
                *i += 2;
            }
            '&' => {
                saw_newline = false;
                if let Some((decoded, next)) = parse_character_reference(chars, *i) {
                    title.push_str(&decoded);
                    *i = next;
                } else {
                    title.push(ch);
                    *i += 1;
                }
            }
            _ => {
                if !ch.is_whitespace() {
                    saw_newline = false;
                }
                title.push(ch);
                *i += 1;
            }
        }
    }
    None
}

/// CommonMark caps a link reference label at 999 characters inside the brackets.
/// Rejecting longer candidates in O(1) — before collecting/normalizing them —
/// keeps a bracket-heavy line linear instead of quadratic (each `[` would
/// otherwise collect its whole bracket span just to fail the lookup).
const MAX_REFERENCE_LABEL_LEN: usize = 999;

/// A reference label is literal source with escaped brackets, not an inline
/// label: backticks and other markup never hide its closing bracket. A malformed
/// or overlong following label does not prevent a preceding shortcut reference.
fn scan_reference_label(
    chars: &[char],
    open: usize,
    work: &mut InlineWorkBudget,
) -> Option<(Range<usize>, usize)> {
    if chars.get(open) != Some(&'[') {
        return None;
    }
    let start = open + 1;
    let mut i = start;
    let mut found = None;
    while i < chars.len() && i - start <= MAX_REFERENCE_LABEL_LEN {
        match chars[i] {
            ']' => {
                found = Some((start..i, i + 1));
                break;
            }
            '[' => break,
            '\\' if chars.get(i + 1).is_some() => i += 2,
            _ => i += 1,
        }
    }
    work.charge(i.saturating_sub(open).saturating_add(1))
        .then_some(found)
        .flatten()
}

fn reference_label_key(label: &[char], work: &mut InlineWorkBudget) -> Option<String> {
    if label.len() > MAX_REFERENCE_LABEL_LEN || !work.charge(label.len().saturating_mul(2)) {
        return None;
    }
    normalize_reference_label_chars(label)
}

fn reference_link_target<'a>(
    chars: &[char],
    open: usize,
    close: usize,
    refs: &'a ReferenceMap,
    work: &mut InlineWorkBudget,
) -> Option<(&'a LinkReference, usize)> {
    if refs.is_empty() {
        return None;
    }
    if let Some((label, next)) = scan_reference_label(chars, close + 1, work) {
        let label = if label.is_empty() {
            reference_label_key(&chars[open + 1..close], work)?
        } else {
            reference_label_key(&chars[label], work)?
        };
        // A syntactically complete full/collapsed reference owns its label even
        // if unresolved. Only malformed following labels allow shortcut fallback.
        return refs.get(&label).map(|reference| (reference, next));
    }
    let label = reference_label_key(&chars[open + 1..close], work)?;
    refs.get(&label).map(|reference| (reference, close + 1))
}

fn parse_reference_link_like(
    chars: &[char],
    i: usize,
    close: Option<usize>,
    refs: &ReferenceMap,
    profiler: &mut ParseProfiler,
    image: bool,
) -> Option<(Vec<Inline>, String, Option<String>, usize)> {
    if chars.get(i) != Some(&'[') {
        return None;
    }
    let close = close?;
    let (reference, next) =
        reference_link_target(chars, i, close, refs, &mut profiler.inline_work)?;
    let mark = profiler.destination_mark();
    let content = parse_inlines_chars_nested(
        &chars[i + 1..close],
        refs,
        InlineContext::LinkLabel,
        profiler,
    );
    let nested_link = !image && contains_link(&content);
    if nested_link {
        profiler.destination_rollback(mark);
    } else if image {
        profiler.destination_tokens_rollback(mark);
    }
    if nested_link {
        return None;
    }
    if let Some(tracker) = &mut profiler.destinations {
        tracker.reference_use(chars, i, close, next);
    }
    Some((
        content,
        reference.dest.clone(),
        reference.title.clone(),
        next,
    ))
}

fn parse_autolink(
    chars: &[char],
    i: usize,
    work: &mut InlineWorkBudget,
) -> Option<(String, String, usize)> {
    if chars.get(i) != Some(&'<') {
        return None;
    }
    // Autolink content runs to the closing `>` and may not contain whitespace,
    // `<`, or ASCII control characters (CommonMark). Find the boundary before
    // allocating, and account for unsuccessful scans as well as accepted links.
    let mut j = i + 1;
    while let Some(&ch) = chars.get(j) {
        if ch == '>' || ch == '<' || ch.is_whitespace() || ch.is_control() {
            break;
        }
        j += 1;
    }
    if !work.charge(j - i) || chars.get(j) != Some(&'>') || j == i + 1 {
        return None;
    }
    let content: String = chars[i + 1..j].iter().collect();
    if is_uri_autolink(&content) {
        // URI autolinks keep the destination verbatim (including `tel:`, `urn:`,
        // `mailto:`, and other opaque schemes that lack `://`).
        Some((content.clone(), content, j + 1))
    } else if is_email_autolink(&content) {
        let dest = format!("mailto:{content}");
        Some((content, dest, j + 1))
    } else {
        None
    }
}

/// A CommonMark absolute-URI autolink scheme: an ASCII letter followed by 1..=31
/// of `[A-Za-z0-9+.-]`, then a `:`. The body after the colon is validated by the
/// caller (no whitespace / `<` / control characters).
fn is_uri_autolink(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.first().is_none_or(|b| !b.is_ascii_alphabetic()) {
        return false;
    }
    let mut k = 1;
    while k < bytes.len() {
        let b = bytes[k];
        if b == b':' {
            // Scheme is `bytes[..k]`; total scheme length must be 2..=32.
            return (2..=32).contains(&k);
        }
        if b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-') {
            k += 1;
        } else {
            return false;
        }
    }
    false
}

/// A CommonMark email autolink (the HTML5 email-address production).
fn is_email_autolink(s: &str) -> bool {
    let Some(at) = s.find('@') else {
        return false;
    };
    let (local, domain) = (&s[..at], &s[at + 1..]);
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    let local_ok = local.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'.' | b'!'
                    | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'/'
                    | b'='
                    | b'?'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'{'
                    | b'|'
                    | b'}'
                    | b'~'
                    | b'-'
            )
    });
    if !local_ok {
        return false;
    }
    // Domain: dot-separated labels of `[A-Za-z0-9-]`, each 1..=63 chars and not
    // starting or ending with `-`.
    domain.split('.').all(|label| {
        let b = label.as_bytes();
        !b.is_empty()
            && b.len() <= 63
            && b[0] != b'-'
            && b[b.len() - 1] != b'-'
            && b.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'-')
    })
}

/// Longest possible body between `&` and `;` for a *valid* character reference.
/// CommonMark 0.31.2 bounds every form: the longest HTML5 named entity is
/// `CounterClockwiseContourIntegral` (31 chars), decimal numeric refs are at
/// most 7 digits (`&#` + 7), and hex at most 6 (`&#x` + 6). Anything longer can
/// never resolve, so we refuse to scan past this window. Capping the `;` search
/// keeps `&`-dense untrusted input linear instead of O(n^2) (each `&` otherwise
/// scanned to end-of-input) without changing the result for any valid input.
const MAX_CHAR_REF_BODY_LEN: usize = 32;

fn parse_character_reference(chars: &[char], i: usize) -> Option<(String, usize)> {
    if chars.get(i) != Some(&'&') {
        return None;
    }
    let slice = chars.get(i + 1..)?;
    let semi_offset = slice
        .iter()
        .take(MAX_CHAR_REF_BODY_LEN + 1)
        .position(|&ch| ch == ';')?;
    if semi_offset == 0 || semi_offset > MAX_CHAR_REF_BODY_LEN {
        return None;
    }
    let body_chars = &slice[..semi_offset];

    if body_chars == ['a', 'm', 'p'] {
        return Some(("&".to_string(), i + 1 + semi_offset + 1));
    }
    if body_chars == ['l', 't'] {
        return Some(("<".to_string(), i + 1 + semi_offset + 1));
    }
    if body_chars == ['g', 't'] {
        return Some((">".to_string(), i + 1 + semi_offset + 1));
    }
    if body_chars == ['q', 'u', 'o', 't'] {
        return Some(("\"".to_string(), i + 1 + semi_offset + 1));
    }
    if body_chars == ['a', 'p', 'o', 's'] {
        return Some(("'".to_string(), i + 1 + semi_offset + 1));
    }
    if body_chars == ['n', 'b', 's', 'p'] {
        return Some(("\u{a0}".to_string(), i + 1 + semi_offset + 1));
    }

    let mut buf = [0u8; MAX_CHAR_REF_BODY_LEN];
    for (idx, &ch) in body_chars.iter().enumerate() {
        if !ch.is_ascii() {
            return None;
        }
        buf[idx] = ch as u8;
    }
    let body_str = std::str::from_utf8(&buf[..body_chars.len()]).ok()?;

    let decoded: String = if let Some(numeric) = body_str
        .strip_prefix("#x")
        .or_else(|| body_str.strip_prefix("#X"))
    {
        decode_numeric_reference(numeric, 16)?.to_string()
    } else if let Some(numeric) = body_str.strip_prefix('#') {
        decode_numeric_reference(numeric, 10)?.to_string()
    } else {
        entities::lookup(body_str)?.to_string()
    };
    Some((decoded, i + 1 + semi_offset + 1))
}

fn parse_bare_url_autolink(
    chars: &[char],
    i: usize,
    work: &mut InlineWorkBudget,
) -> Option<(String, String, usize)> {
    if work.exhausted || !bare_url_left_boundary(chars, i) {
        return None;
    }

    let is_www = match chars.get(i) {
        Some('h' | 'H')
            if starts_with_ascii_chars_ignore_case(chars, i, b"http://")
                || starts_with_ascii_chars_ignore_case(chars, i, b"https://") =>
        {
            false
        }
        Some('w' | 'W') if starts_with_ascii_chars_ignore_case(chars, i, b"www.") => true,
        _ => return None,
    };

    let mut end = i;
    while end < chars.len() && !chars[end].is_whitespace() && chars[end] != '<' && chars[end] != '>'
    {
        end += 1;
    }
    // Include the forward scan and the suffix's parenthesis/trim passes. Failed
    // angle-autolink attempts must share the same bound as accepted candidates.
    if !work.charge(end.saturating_sub(i).saturating_mul(3).saturating_add(1)) {
        return None;
    }
    end = trim_bare_url_suffix(chars, i, end);
    if end == i || (is_www && end <= i + 4) {
        return None;
    }

    if bare_autolink_is_angle_attempt(chars, i, end, work)? {
        return None;
    }

    let label = chars[i..end].iter().collect::<String>();
    let dest = if is_www {
        format!("http://{label}")
    } else {
        label.clone()
    };
    Some((label, dest, end))
}

/// Parse a bare (scheme-less) email address into a `mailto:` autolink, GFM-style.
///
/// Returns `(label, dest, end)` where `label` is the matched address and `dest`
/// is `mailto:<label>`. Conservative: the local part is alphanumeric plus
/// `. - _ +`, the domain is dot-separated alphanumeric/`-`/`_` labels with at
/// least one dot, trailing sentence dots are trimmed, and the domain may not end
/// in `-`/`_` — so ordinary `@`-containing prose is not falsely linked.
fn parse_bare_email_autolink(
    chars: &[char],
    i: usize,
    work: &mut InlineWorkBudget,
) -> Option<(String, String, usize)> {
    if work.exhausted || !bare_url_left_boundary(chars, i) {
        return None;
    }
    let mut j = i;
    while chars
        .get(j)
        .is_some_and(|&c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
    {
        j += 1;
    }
    // A rejected local part can be retried after every underscore. Charge it
    // before any early return, even when the paragraph's only '@' is elsewhere.
    if !work.charge(j.saturating_sub(i).saturating_add(1)) {
        return None;
    }
    // Need a non-empty local part immediately followed by `@`.
    if j == i || chars.get(j) != Some(&'@') {
        return None;
    }
    j += 1;
    let domain_start = j;
    while chars
        .get(j)
        .is_some_and(|&c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        j += 1;
    }
    // Cover the domain scan, trailing-dot trim and dotted-domain validation.
    if !work.charge(
        j.saturating_sub(domain_start)
            .saturating_mul(3)
            .saturating_add(1),
    ) {
        return None;
    }
    // Trim trailing dots (sentence punctuation), then require a real dotted domain
    // that does not end in `-`/`_`.
    let mut end = j;
    while end > domain_start && chars[end - 1] == '.' {
        end -= 1;
    }
    if end <= domain_start || matches!(chars[end - 1], '-' | '_') {
        return None;
    }
    if !chars[domain_start..end].contains(&'.') {
        return None;
    }

    if bare_autolink_is_angle_attempt(chars, i, end, work)? {
        return None;
    }

    let label: String = chars[i..end].iter().collect();
    let dest = format!("mailto:{label}");
    Some((label, dest, end))
}

/// Unescaped `< ... >` belongs to the angle-autolink grammar, even when that
/// grammar rejects its whitespace. Keep the URL and email rules identical and
/// account for whitespace/escape lookaround in the shared speculative budget.
fn bare_autolink_is_angle_attempt(
    chars: &[char],
    start: usize,
    end: usize,
    work: &mut InlineWorkBudget,
) -> Option<bool> {
    let mut prev_non_ws = start;
    while prev_non_ws > 0 && chars[prev_non_ws - 1].is_whitespace() {
        prev_non_ws -= 1;
    }
    if !work.charge(start.saturating_sub(prev_non_ws).saturating_add(1)) {
        return None;
    }
    if prev_non_ws > 0
        && chars[prev_non_ws - 1] == '<'
        && !is_char_escaped(chars, prev_non_ws - 1, work)?
    {
        let mut next_non_ws = end;
        while next_non_ws < chars.len() && chars[next_non_ws].is_whitespace() {
            next_non_ws += 1;
        }
        if !work.charge(next_non_ws.saturating_sub(end).saturating_add(1)) {
            return None;
        }
        if next_non_ws < chars.len()
            && chars[next_non_ws] == '>'
            && !is_char_escaped(chars, next_non_ws, work)?
        {
            return Some(true);
        }
    }
    Some(false)
}

#[inline(always)]
fn starts_with_ascii_chars_ignore_case(chars: &[char], i: usize, needle: &[u8]) -> bool {
    let Some(end) = i.checked_add(needle.len()) else {
        return false;
    };
    let Some(window) = chars.get(i..end) else {
        return false;
    };
    for idx in 0..needle.len() {
        let got = window[idx];
        if !got.is_ascii() || !(got as u8).eq_ignore_ascii_case(&needle[idx]) {
            return false;
        }
    }
    true
}

fn is_char_escaped(chars: &[char], idx: usize, work: &mut InlineWorkBudget) -> Option<bool> {
    let mut bs = 0;
    let mut p = idx;
    while p > 0 && chars[p - 1] == '\\' {
        bs += 1;
        p -= 1;
    }
    work.charge(bs + 1).then_some(bs % 2 == 1)
}

fn bare_url_left_boundary(chars: &[char], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    chars.get(i - 1).is_some_and(|ch| {
        ch.is_whitespace() || matches!(ch, '(' | '[' | '{' | '"' | '\'' | '*' | '_' | '~')
    })
}

const fn bare_url_trailing_punctuation(ch: char) -> bool {
    matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | '*' | '_' | '~')
}

/// GFM autolinks preserve punctuation in the interior, including Markdown
/// delimiters, while excluding sentence/formatting suffixes and unmatched ')'.
/// Count parentheses once, then trim in one backward pass: alternating ')~'
/// suffixes must not re-scan the whole URL for every removed character.
fn trim_bare_url_suffix(chars: &[char], start: usize, mut end: usize) -> usize {
    while end > start && bare_url_trailing_punctuation(chars[end - 1]) {
        end -= 1;
    }
    if end <= start || chars[end - 1] != ')' {
        return end;
    }
    let (opens, closes) = chars[start..end]
        .iter()
        .fold((0usize, 0usize), |counts, ch| match ch {
            '(' => (counts.0 + 1, counts.1),
            ')' => (counts.0, counts.1 + 1),
            _ => counts,
        });
    let mut excess = closes.saturating_sub(opens);
    while end > start {
        if bare_url_trailing_punctuation(chars[end - 1]) {
            end -= 1;
        } else if chars[end - 1] == ')' && excess > 0 {
            end -= 1;
            excess -= 1;
        } else {
            break;
        }
    }
    end
}

fn decode_numeric_reference(value: &str, radix: u32) -> Option<char> {
    if value.is_empty() {
        return None;
    }
    if radix == 10 && value.len() > 7 {
        return None;
    }
    if radix == 16 && value.len() > 6 {
        return None;
    }
    // Only an all-digits run (in the given radix) is a numeric reference at all;
    // verify bytes directly without UTF-8 char iterator overhead.
    for &b in value.as_bytes() {
        if !(b as char).is_digit(radix) {
            return None;
        }
    }
    // CommonMark: the NUL code point (U+0000), out-of-range values (> U+10FFFF,
    // including digit runs that overflow `u32`), and surrogate code points
    // (U+D800..=U+DFFF) all decode to the replacement character U+FFFD rather
    // than emitting a raw/invalid byte. `char::from_u32` already rejects
    // surrogates and out-of-range scalars; we additionally fold U+0000 so a
    // literal NUL never reaches the output.
    let code = u32::from_str_radix(value, radix).unwrap_or(u32::MAX);
    Some(
        char::from_u32(code)
            .filter(|&c| c != '\0')
            .unwrap_or('\u{FFFD}'),
    )
}

fn is_html_tag_name_start(c: char) -> bool {
    c.is_ascii_alphabetic()
}

fn is_html_tag_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

fn is_html_attr_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == ':'
}

fn is_html_attr_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-')
}

fn is_html_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0C')
}

fn parse_inline_html(
    chars: &[char],
    i: usize,
    work: &mut InlineWorkBudget,
) -> Option<(String, usize)> {
    if work.exhausted {
        return None;
    }
    let result = scan_inline_html_end(chars, i);
    let scanned = match result {
        Ok(end) | Err(end) => end.saturating_sub(i),
    };
    // Attribute whitespace can be inspected twice when '=' is absent.
    if !work.charge(scanned.saturating_mul(2)) {
        return None;
    }
    let next = result.ok()?;
    Some((chars[i..next].iter().collect(), next))
}

/// Return the end of a tag, or the furthest inspected offset on failure.
/// Returning the cursor even for malformed tags lets the caller bound repeated
/// scans without imposing a fixed length cap on valid HTML attributes.
fn scan_inline_html_end(chars: &[char], i: usize) -> Result<usize, usize> {
    if chars.get(i) != Some(&'<') {
        return Err(i);
    }
    let n = chars.len();
    // 1. Comment: <!-- ... -->
    if chars.get(i + 1) == Some(&'!')
        && chars.get(i + 2) == Some(&'-')
        && chars.get(i + 3) == Some(&'-')
    {
        let mut j = i + 4;
        while j + 2 < n {
            if chars[j] == '-' && chars[j + 1] == '-' && chars[j + 2] == '>' {
                return Ok(j + 3);
            }
            j += 1;
        }
        return Err(j);
    }
    // 2. Processing instruction: <? ... ?>
    if chars.get(i + 1) == Some(&'?') {
        let mut j = i + 2;
        while j + 1 < n {
            if chars[j] == '?' && chars[j + 1] == '>' {
                return Ok(j + 2);
            }
            j += 1;
        }
        return Err(j);
    }
    // 3. CDATA: <![CDATA[ ... ]]>
    if i + 8 < n && chars[i + 1..i + 9].iter().collect::<String>() == "![CDATA[" {
        let mut j = i + 9;
        while j + 2 < n {
            if chars[j] == ']' && chars[j + 1] == ']' && chars[j + 2] == '>' {
                return Ok(j + 3);
            }
            j += 1;
        }
        return Err(j);
    }
    // 4. Declaration: <!ASCII_UPPER ... >
    if chars.get(i + 1) == Some(&'!') && chars.get(i + 2).is_some_and(|c| c.is_ascii_uppercase()) {
        let mut j = i + 3;
        while j < n && chars[j] != '>' {
            j += 1;
        }
        if j < n && chars[j] == '>' {
            return Ok(j + 1);
        }
        return Err(j);
    }
    // 5. Closing tag: </tag_name whitespace* >
    if chars.get(i + 1) == Some(&'/') {
        let mut j = i + 2;
        if j >= n || !is_html_tag_name_start(chars[j]) {
            return Err(j);
        }
        while j < n && is_html_tag_name_char(chars[j]) {
            j += 1;
        }
        while j < n && is_html_whitespace(chars[j]) {
            j += 1;
        }
        if j < n && chars[j] == '>' {
            return Ok(j + 1);
        }
        return Err(j);
    }
    // 6. Open tag: <tag_name (whitespace+ attr)* whitespace* /? >
    let mut j = i + 1;
    if j >= n || !is_html_tag_name_start(chars[j]) {
        return Err(j);
    }
    while j < n && is_html_tag_name_char(chars[j]) {
        j += 1;
    }
    loop {
        let ws_start = j;
        while j < n && is_html_whitespace(chars[j]) {
            j += 1;
        }
        let had_ws = j > ws_start;
        if j < n && chars[j] == '/' {
            j += 1;
            while j < n && is_html_whitespace(chars[j]) {
                j += 1;
            }
            if j < n && chars[j] == '>' {
                return Ok(j + 1);
            }
            return Err(j);
        }
        if j < n && chars[j] == '>' {
            return Ok(j + 1);
        }
        if !had_ws || j >= n || !is_html_attr_name_start(chars[j]) {
            return Err(j);
        }
        while j < n && is_html_attr_name_char(chars[j]) {
            j += 1;
        }
        let before_eq = j;
        while j < n && is_html_whitespace(chars[j]) {
            j += 1;
        }
        if j < n && chars[j] == '=' {
            j += 1;
            while j < n && is_html_whitespace(chars[j]) {
                j += 1;
            }
            if j >= n {
                return Err(j);
            }
            if chars[j] == '"' {
                j += 1;
                while j < n && chars[j] != '"' {
                    j += 1;
                }
                if j >= n || chars[j] != '"' {
                    return Err(j);
                }
                j += 1;
            } else if chars[j] == '\'' {
                j += 1;
                while j < n && chars[j] != '\'' {
                    j += 1;
                }
                if j >= n || chars[j] != '\'' {
                    return Err(j);
                }
                j += 1;
            } else {
                let val_start = j;
                while j < n
                    && !is_html_whitespace(chars[j])
                    && !matches!(chars[j], '"' | '\'' | '=' | '<' | '>' | '`')
                {
                    j += 1;
                }
                if j == val_start {
                    return Err(j);
                }
            }
        } else {
            j = before_eq;
        }
    }
}

fn find_closing_bracket(chars: &[char], open: usize) -> Option<usize> {
    if chars.get(open) != Some(&'[') {
        return None;
    }
    let mut depth = 1;
    let mut i = open + 1;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Pair source brackets within a link label, where GFM bare autolinks are
/// disabled. Opaque inline atoms and accepted link targets own their source. The
/// stack walks the label once; speculative recognizers share the inline work
/// budget, including failed scans. A lookup is O(1).
///
/// The table is 4 bytes per char (`u32` + sentinel) instead of the 16 of
/// `Option<usize>` whenever the slice fits `u32` indices — every reachable
/// input (the CLI caps files at 64 MiB; wasm32 cannot address a slice past
/// `u32::MAX`), while unbounded library callers beyond that keep the
/// full-width entries (pass-5 discipline: guard the domain, keep a full-width
/// fallback). The pairing loop lives once in
/// [`compute_bracket_pairs_in`](compute_bracket_pairs_in) and both
/// representations monomorphize from that single source, so pairing decisions
/// are identical by construction.
#[derive(Debug)]
enum BracketPairs {
    /// `u32::MAX` marks an unpaired `[`; routing guarantees every recorded
    /// close index is `< chars.len() <= u32::MAX`, so the sentinel cannot
    /// collide with a real index.
    Compact(Vec<u32>),
    /// Fallback for slices longer than `u32::MAX` chars (16 bytes per char,
    /// exactly the pre-pass-25 representation).
    Wide(Vec<Option<usize>>),
}

/// Sentinel stored in the compact table for a `[` with no closing `]`.
const UNPAIRED_BRACKET: u32 = u32::MAX;

impl BracketPairs {
    /// The `]` index closing the `[` at `open`, if any — value-for-value the
    /// entry the old `Vec<Option<usize>>` held at `open` (out-of-bounds and
    /// unpaired both map to `None`).
    fn partner(&self, open: usize) -> Option<usize> {
        match self {
            BracketPairs::Compact(table) => match table.get(open) {
                None | Some(&UNPAIRED_BRACKET) => None,
                Some(&close) => Some(close as usize),
            },
            BracketPairs::Wide(table) => table.get(open).copied().flatten(),
        }
    }
}

/// Storage half of the pairing pass: how each representation starts all-unpaired,
/// records one pair, and reads one back. Pairing decisions stay in the shared
/// generic loop, including the allocation-free single-candidate representation.
trait PairTable {
    fn unpaired(len: usize) -> Self;
    fn record(&mut self, open: usize, close: usize);
}

/// 4 bytes per char; domain `len <= u32::MAX` (enforced by
/// [`compute_bracket_pairs`], so `record`'s narrowing cast is lossless).
struct CompactTable(Vec<u32>);

/// 16 bytes per char; works for any `len`.
struct WideTable(Vec<Option<usize>>);

impl PairTable for CompactTable {
    fn unpaired(len: usize) -> Self {
        CompactTable(vec![UNPAIRED_BRACKET; len])
    }
    fn record(&mut self, open: usize, close: usize) {
        // Lossless: close < len <= u32::MAX on this arm.
        self.0[open] = close as u32;
    }
}

impl PairTable for WideTable {
    fn unpaired(len: usize) -> Self {
        WideTable(vec![None; len])
    }
    fn record(&mut self, open: usize, close: usize) {
        self.0[open] = Some(close);
    }
}

/// A prose probe needs only its root pair. Nested pairs are overwritten, and
/// the scanner stops as soon as it records the requested root's closing bracket.
struct SinglePair(Option<(usize, usize)>);

impl PairTable for SinglePair {
    fn unpaired(_len: usize) -> Self {
        SinglePair(None)
    }
    fn record(&mut self, open: usize, close: usize) {
        self.0 = Some((open, close));
    }
}

/// Start at the actual prose tokenizer's candidate, never at punctuation in a
/// consumed URL. Successful neighboring links scan disjoint labels. Repeated
/// rejected or unmatched candidates charge every walk to the shared work budget.
fn find_link_label_close(
    chars: &[char],
    start: usize,
    open: usize,
    refs: &ReferenceMap,
    work: &mut InlineWorkBudget,
) -> Option<usize> {
    compute_bracket_pairs_in::<SinglePair>(chars, refs, work, start, Some(open))
        .0
        .filter(|&(index, _)| index == open)
        .map(|(_, close)| close)
}

struct OpenBracket {
    index: usize,
    image: bool,
    may_form_link: bool,
    contains_link: bool,
}

/// `S` chooses only the table storage width. Matching recognizes the same
/// opaque atoms and target grammar as the tokenizer. Accepted inner links make
/// an outer link invalid; an accepted image instead flattens its label, so links
/// inside it must not invalidate an enclosing link.
fn compute_bracket_pairs_in<S: PairTable>(
    chars: &[char],
    refs: &ReferenceMap,
    work: &mut InlineWorkBudget,
    start: usize,
    root: Option<usize>,
) -> S {
    let mut pairs = S::unpaired(chars.len());
    let capacity = if root.is_some() { 4 } else { chars.len() / 64 };
    let mut open_stack: Vec<OpenBracket> = Vec::with_capacity(capacity);
    let mut i = start;
    while i < chars.len() && work.charge(1) {
        match chars[i] {
            '\\' => i += 1,
            '`' => {
                let Some(n) = run_len(chars, i, '`', work) else {
                    break;
                };
                if let Some(end) = find_code_close(chars, i + n, '`', n, work) {
                    i = end + n - 1;
                } else {
                    i += n - 1;
                }
            }
            '$' => {
                let Some(n) = run_len(chars, i, '$', work) else {
                    break;
                };
                if let Some((_, next)) =
                    parse_math_delim(chars, i, if n >= 2 { 2 } else { 1 }, work)
                {
                    i = next - 1;
                }
            }
            '<' => {
                if let Some((_, _, next)) = parse_autolink(chars, i, work) {
                    if let Some(parent) = open_stack.last_mut() {
                        parent.contains_link = true;
                    }
                    i = next - 1;
                } else if let Some((_, next)) = parse_inline_html(chars, i, work) {
                    i = next - 1;
                }
            }
            '!' if chars.get(i + 1) == Some(&'[') => {
                // This branch, rather than inspecting the preceding character
                // at '[', distinguishes a real image from an escaped literal '!'.
                i += 1;
                open_stack.push(OpenBracket {
                    index: i,
                    image: true,
                    may_form_link: true,
                    contains_link: false,
                });
            }
            '[' => {
                let footnote = chars.get(i + 1) == Some(&'^');
                if footnote && let Some((_, next)) = parse_footnote_ref_id(chars, i + 2) {
                    i = next - 1;
                } else {
                    open_stack.push(OpenBracket {
                        index: i,
                        image: false,
                        may_form_link: !footnote,
                        contains_link: false,
                    });
                }
            }
            ']' => {
                if let Some(open) = open_stack.pop() {
                    pairs.record(open.index, i);
                    if root == Some(open.index) {
                        break;
                    }
                    let next = if open.may_form_link && (open.image || !open.contains_link) {
                        parse_inline_link_target(chars, i, work)
                            .map(|target| target.next)
                            .or_else(|| {
                                reference_link_target(chars, open.index, i, refs, work)
                                    .map(|(_, next)| next)
                            })
                    } else {
                        None
                    };
                    let contains_link = if next.is_some() {
                        !open.image
                    } else {
                        open.contains_link
                    };
                    if contains_link && let Some(parent) = open_stack.last_mut() {
                        parent.contains_link = true;
                    }
                    if let Some(next) = next {
                        i = next - 1;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    pairs
}

fn compute_bracket_pairs(
    chars: &[char],
    refs: &ReferenceMap,
    work: &mut InlineWorkBudget,
) -> BracketPairs {
    // Indices are < len, so len <= u32::MAX keeps every recorded index at
    // u32::MAX - 1 or below — the sentinel stays collision-free even at the
    // boundary. On wasm32 (usize == u32) the comparison is tautologically true
    // and Compact is the only live arm.
    if chars.len() <= u32::MAX as usize {
        BracketPairs::Compact(
            compute_bracket_pairs_in::<CompactTable>(chars, refs, work, 0, None).0,
        )
    } else {
        BracketPairs::Wide(compute_bracket_pairs_in::<WideTable>(chars, refs, work, 0, None).0)
    }
}

fn normalize_reference_label_chars(label: &[char]) -> Option<String> {
    // Definitions and uses share the source-character limit, before trimming
    // whitespace or case folding. Invalid definitions must stay visible.
    if label.len() > MAX_REFERENCE_LABEL_LEN {
        return None;
    }
    let mut escaped = false;
    for &ch in label {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '[' || ch == ']' {
            return None;
        }
    }
    let mut start = 0usize;
    while start < label.len() && label[start].is_whitespace() {
        start += 1;
    }
    let mut end = label.len();
    while end > start && label[end - 1].is_whitespace() {
        end -= 1;
    }

    let mut out = String::with_capacity(end.saturating_sub(start));
    let mut pending_space = false;
    for &ch in &label[start..end] {
        if ch.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        unicode_casefold::push(ch, &mut out);
        pending_space = false;
    }
    if out.is_empty() { None } else { Some(out) }
}

fn skip_spaces(chars: &[char], i: &mut usize) {
    while *i < chars.len() && (chars[*i] == ' ' || chars[*i] == '\t') {
        *i += 1;
    }
}

fn inlines_to_plain(inlines: &[Inline]) -> String {
    let mut s = String::new();
    push_inlines_to_plain(inlines, &mut s);
    s
}

fn push_inlines_to_plain(inlines: &[Inline], out: &mut String) {
    for inl in inlines {
        match inl {
            Inline::Text(t) | Inline::Code(t) | Inline::Math(t) | Inline::DisplayMath(t) => {
                provenance::append(out, t)
            }
            Inline::Emphasis(c) | Inline::Strong(c) | Inline::Strikethrough(c) => {
                push_inlines_to_plain(c, out);
            }
            Inline::Link { content, .. } => push_inlines_to_plain(content, out),
            Inline::Image { alt, .. } => provenance::append(out, alt),
            Inline::FootnoteRef { id } => {
                provenance::append(out, &provenance::visible_footnote(id))
            }
            Inline::SoftBreak | Inline::HardBreak => provenance::append(out, " "),
            Inline::Html(h) => provenance::append(out, h),
        }
    }
}

// ---- small helpers ----------------------------------------------------------

/// Width, in columns, of a line's leading indentation. Tabs advance to the next
/// 4-column tab stop (CommonMark), so a single leading tab counts as the four
/// columns that make a line indented code. Only the leading run is measured;
/// tabs elsewhere on the line are left for the content to keep verbatim.
fn leading_spaces(line: &str) -> usize {
    let mut col = 0usize;
    for ch in line.chars() {
        match ch {
            ' ' => col += 1,
            '\t' => col += 4 - col % 4,
            _ => break,
        }
    }
    col
}

/// Strip up to `n` columns of leading indentation from a fenced code block's
/// content line, matching the opening fence's indent (CommonMark). Spaces are
/// one column each; a leading tab (advancing to the next 4-column stop) is only
/// removed whole, so it is left intact when fewer columns than it spans remain
/// to strip — a partial tab is never split.
fn strip_fence_indent(line: &str, n: usize) -> &str {
    if n == 0 {
        return line;
    }
    let mut col = 0usize;
    let mut byte = 0usize;
    for ch in line.chars() {
        match ch {
            ' ' if col < n => {
                col += 1;
                byte += 1;
            }
            '\t' if col + (4 - col % 4) <= n => {
                col += 4 - col % 4;
                byte += 1;
            }
            _ => break,
        }
    }
    &line[byte..]
}

#[inline(always)]
fn trim_space_tab(s: &str) -> &str {
    trim_start_space_tab(trim_end_space_tab(s))
}

/// CommonMark blank line: only U+0020 and U+0009. Unicode `trim()` also strips
/// NBSP/Zs and would drop a line that is only `\u{00A0}`.
#[inline(always)]
fn is_blank_line(line: &str) -> bool {
    trim_space_tab(line).is_empty()
}

/// Byte-scan form of `s.trim_start_matches(is_space_or_tab)`: `' '` and `'\t'`
/// are single-byte chars and no multi-byte UTF-8 sequence can contain either
/// byte (continuation/lead bytes are >= 0x80), so skipping exactly those bytes
/// from the front lands on the same char boundary the char-pattern trim does —
/// without decoding each char.
#[inline(always)]
fn trim_start_space_tab(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && is_space_or_tab_byte(bytes[i]) {
        i += 1;
    }
    debug_assert!(s.is_char_boundary(i));
    &s[i..]
}

/// Byte-scan form of `s.trim_end_matches(is_space_or_tab)`; same equivalence
/// argument as [`trim_start_space_tab`], mirrored: the first non-space/tab byte
/// met from the right can only be the last byte of a char the char-pattern trim
/// would also stop at, so `end` stays a char boundary.
#[inline(always)]
fn trim_end_space_tab(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    while end > 0 && is_space_or_tab_byte(bytes[end - 1]) {
        end -= 1;
    }
    debug_assert!(s.is_char_boundary(end));
    &s[..end]
}

/// ASCII bytes that are Unicode whitespace (White_Space): U+0009..U+000D and
/// U+0020. This is the whole ASCII part of `char::is_whitespace`.
const UNICODE_WS_ASCII_TABLE: [bool; 256] = {
    let mut table = [false; 256];
    table[0x09] = true;
    table[0x0A] = true;
    table[0x0B] = true;
    table[0x0C] = true;
    table[0x0D] = true;
    table[0x20] = true;
    table
};

#[inline(always)]
const fn is_unicode_ws_ascii(byte: u8) -> bool {
    UNICODE_WS_ASCII_TABLE[byte as usize]
}

/// If a multi-byte Unicode-whitespace char starts at `i`, return its byte
/// length. The complete non-ASCII White_Space set, as UTF-8 bytes:
/// U+0085 (C2 85), U+00A0 (C2 A0), U+1680 (E1 9A 80), U+2000..U+200A
/// (E2 80 80..8A), U+2028/U+2029 (E2 80 A8/A9), U+202F (E2 80 AF),
/// U+205F (E2 81 9F), U+3000 (E3 80 80). Each sequence is the unique UTF-8
/// encoding of its codepoint, and no other codepoint encodes to these bytes,
/// so matching bytes decides whitespace exactly like `char::is_whitespace`
/// without decoding. (U+200B and U+FEFF are NOT White_Space — deliberately
/// absent, and pinned by tests.)
#[inline(always)]
fn multibyte_unicode_ws_len(bytes: &[u8], i: usize) -> Option<usize> {
    match bytes.get(i) {
        Some(0xC2) => match bytes.get(i + 1) {
            Some(0x85) | Some(0xA0) => Some(2),
            _ => None,
        },
        Some(0xE1) => {
            if bytes.get(i + 1) == Some(&0x9A) && bytes.get(i + 2) == Some(&0x80) {
                Some(3)
            } else {
                None
            }
        }
        Some(0xE2) => match bytes.get(i + 1) {
            Some(&0x80) => match bytes.get(i + 2) {
                Some(0x80..=0x8A) | Some(0xA8) | Some(0xA9) | Some(0xAF) => Some(3),
                _ => None,
            },
            Some(&0x81) if bytes.get(i + 2) == Some(&0x9F) => Some(3),
            _ => None,
        },
        Some(0xE3) => {
            if bytes.get(i + 1) == Some(&0x80) && bytes.get(i + 2) == Some(&0x80) {
                Some(3)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Byte-level `str::trim_start()`: skip Unicode whitespace from the front
/// without decoding chars. Exactly equivalent to
/// `s.trim_start_matches(char::is_whitespace)` on every valid UTF-8 input:
/// ASCII whitespace bytes are matched by table, and a lead byte is matched only
/// when the full multi-byte sequence is one of the 19 non-ASCII whitespace
/// chars; every other char stops the scan at the boundary it starts on.
fn trim_start_unicode_ws(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte < 0x80 {
            if !is_unicode_ws_ascii(byte) {
                break;
            }
            i += 1;
        } else {
            match multibyte_unicode_ws_len(bytes, i) {
                Some(len) => i += len,
                None => break,
            }
        }
    }
    debug_assert!(s.is_char_boundary(i));
    &s[i..]
}

/// Byte-level `str::trim_end()`: skip Unicode whitespace from the back. ASCII
/// whitespace bytes are single-byte chars; the first byte >= 0x80 met from the
/// right is the last byte of a multi-byte char, whose start is found by walking
/// back over continuation bytes (0b10xxxxxx) — the whole char is then removed
/// only if it is one of the 19 non-ASCII whitespace chars. Equivalence to
/// `s.trim_end_matches(char::is_whitespace)` is pinned exhaustively by tests.
fn trim_end_unicode_ws(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    while end > 0 {
        let byte = bytes[end - 1];
        if byte < 0x80 {
            if !is_unicode_ws_ascii(byte) {
                break;
            }
            end -= 1;
            continue;
        }
        let mut start = end - 1;
        while start > 0 && (bytes[start] & 0xC0) == 0x80 {
            start -= 1;
        }
        if multibyte_unicode_ws_len(bytes, start) == Some(end - start) {
            end = start;
        } else {
            break;
        }
    }
    debug_assert!(s.is_char_boundary(end));
    &s[..end]
}

/// Byte-level `str::trim()`: exactly `s.trim_matches(char::is_whitespace)`.
/// Ordering of the start/end scans is immaterial — both bound the same
/// first/last non-whitespace chars — matching std's `trim_matches` result.
fn trim_unicode_ws(s: &str) -> &str {
    trim_start_unicode_ws(trim_end_unicode_ws(s))
}

fn starts_space_or_tab(s: &str) -> bool {
    s.as_bytes()
        .first()
        .is_some_and(|&byte| is_space_or_tab_byte(byte))
}

fn is_space_or_tab(ch: char) -> bool {
    ch == ' ' || ch == '\t'
}

fn is_space_or_tab_byte(byte: u8) -> bool {
    byte == b' ' || byte == b'\t'
}

/// Remove up to `n` columns of leading indentation, expanding tabs to 4-column
/// tab stops. A tab that would straddle the `n`-column boundary is left intact
/// (we never split a tab into spaces, so the result stays a borrowed slice).
fn strip_n(line: &str, n: usize) -> &str {
    let mut col = 0usize;
    for (idx, ch) in line.char_indices() {
        if col >= n {
            return &line[idx..];
        }
        match ch {
            ' ' => col += 1,
            '\t' => {
                let next = col + (4 - col % 4);
                if next > n {
                    return &line[idx..];
                }
                col = next;
            }
            _ => return &line[idx..],
        }
    }
    ""
}

fn is_ascii_punct(c: char) -> bool {
    c.is_ascii_punctuation()
}

fn register_parts(output: &str, parts: &[(usize, &str)], profiler: &ParseProfiler) {
    let mut origins = Vec::new();
    for (offset, part) in parts {
        origins.extend(
            provenance::text_origins(part, profiler)
                .into_iter()
                .map(|mut origin| {
                    origin.bytes.start += offset;
                    origin.bytes.end += offset;
                    origin
                }),
        );
        let newline = offset + part.len();
        if newline < output.len()
            && output.as_bytes()[newline] == b'\n'
            && let Some(source) = profiler
                .destinations
                .as_ref()
                .and_then(|tracker| tracker.line_ending(part))
        {
            origins.push(provenance::Origin {
                bytes: newline..newline + 1,
                source,
            });
        }
    }
    provenance::register(output, origins);
}

fn code_origins(output: &str, chars: &[char], profiler: &ParseProfiler) -> Vec<provenance::Origin> {
    let strip = chars.len() >= 2
        && matches!(chars[0], ' ' | '\n')
        && matches!(chars[chars.len() - 1], ' ' | '\n')
        && chars.iter().any(|c| !c.is_whitespace());
    let body = if strip {
        &chars[1..chars.len() - 1]
    } else {
        chars
    };
    let mut origins = Vec::new();
    let mut offset = 0;
    let mut index = 0;
    while index < body.len() {
        let escaped_pipe = profiler.table_cell_code_escapes
            && body[index] == '\\'
            && body.get(index + 1) == Some(&'|');
        let count = if escaped_pipe { 2 } else { 1 };
        let width = if escaped_pipe || body[index] == '\n' {
            1
        } else {
            body[index].len_utf8()
        };
        if let Some(tracker) = &profiler.destinations {
            for scalar in index..index + count {
                if let Some(source) = tracker.char_range(body, scalar..scalar + 1) {
                    origins.push(provenance::Origin {
                        bytes: offset..offset + width,
                        source,
                    });
                }
            }
        }
        offset += width;
        index += count;
    }
    debug_assert_eq!(offset, output.len());
    origins
}
