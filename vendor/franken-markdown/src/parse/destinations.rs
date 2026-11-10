//! Exact source tokens for destinations recognized by the normal parser.
//!
//! Tracking is opt-in. The ordinary AST stays unchanged; borrowed source slices
//! and the parser's explicitly copied text retain byte coordinates only while
//! this collector is active. There is no second Markdown grammar or source-text
//! search, so equal-looking links in different includes keep distinct origins.

use super::*;
#[path = "source_prefix.rs"]
mod source_prefix;
use std::collections::BTreeMap;

/// One exact destination token in the source passed to [`super::destination_spans`].
/// A token can supply an inline link, image or reference definition; its source
/// range alone does not establish which semantic nodes use that destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationSpan {
    /// The complete source token, including angle delimiters when present.
    pub range: Range<usize>,
    /// The parser-decoded destination, without a title or angle delimiters.
    pub dest: String,
}

impl DestinationSpan {
    /// Return the original UTF-8 byte range after the first decoded `#`, without
    /// an angle token's closing `>`. The separator's original spelling and every
    /// path/query byte stay outside this range, including `\#`, `&#35;` and
    /// `&num;`. Percent escapes are URI data and are not decoded here.
    ///
    /// Uses the normal parser's escape and character-reference decoder. Returns
    /// `None` for invalid/stale token bounds, a decoded value different from
    /// `dest`, or a destination without a fragment separator. This checks token
    /// spelling, not its semantic role or the caller's source revision identity.
    #[must_use]
    pub fn fragment_range(&self, source: &str) -> Option<Range<usize>> {
        let raw = source.get(self.range.clone())?;
        let (body, start, end) = if raw.starts_with('<') {
            (
                raw.strip_prefix('<')?.strip_suffix('>')?,
                self.range.start + 1,
                self.range.end - 1,
            )
        } else {
            (raw, self.range.start, self.range.end)
        };
        let chars: Vec<_> = body.chars().collect();
        let mut decoded = String::with_capacity(body.len());
        let mut index = 0;
        let mut byte_offset = 0;
        let mut fragment = None;
        while index < chars.len() {
            let previous = index;
            let hash = match chars[index] {
                '\\' if chars.get(index + 1).is_some_and(|&ch| is_ascii_punct(ch)) => {
                    let ch = chars[index + 1];
                    decoded.push(ch);
                    index += 2;
                    ch == '#'
                }
                '&' => {
                    if let Some((text, next)) = parse_character_reference(&chars, index) {
                        // No current multi-scalar entity contains '#'. Refuse a
                        // future mixed expansion rather than invent its suffix.
                        if text.contains('#') && text != "#" {
                            return None;
                        }
                        let hash = text == "#";
                        decoded.push_str(&text);
                        index = next;
                        hash
                    } else {
                        decoded.push('&');
                        index += 1;
                        false
                    }
                }
                ch => {
                    decoded.push(ch);
                    index += 1;
                    ch == '#'
                }
            };
            byte_offset += chars[previous..index]
                .iter()
                .map(|ch| ch.len_utf8())
                .sum::<usize>();
            if fragment.is_none() && hash {
                fragment = Some(start + byte_offset);
            }
        }
        (decoded == self.dest)
            .then_some(fragment)
            .flatten()
            .map(|start| start..end)
    }
}

/// Internal source edits need the token's syntax, while the public Markdown
/// destination API deliberately retains its existing recognition contract.
pub(crate) struct SourceDestination {
    pub token: DestinationSpan,
    pub syntax: DestinationSyntax,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DestinationSyntax {
    Markdown,
    HtmlAttribute,
    HtmlAttributeContent,
}

#[derive(Clone)]
struct TextRun {
    copied: Range<usize>,
    original: Range<usize>,
}

struct TextMap {
    len: usize,
    runs: Vec<TextRun>,
}

struct CharRun {
    chars: Range<usize>,
    original: Range<usize>,
}

struct CharMap {
    address: usize,
    widths: Vec<u8>,
    runs: Vec<CharRun>,
}

impl CharMap {
    fn push(&mut self, width: usize, original: Option<Range<usize>>) {
        let index = self.widths.len();
        self.widths.push(width as u8);
        let Some(original) = original else {
            return;
        };
        if let Some(last) = self.runs.last_mut() {
            // Byte checkpoints bound conversion from a char index while keeping
            // tracking much smaller than an Option<Range<usize>> per character.
            if last.chars.end == index
                && last.original.end == original.start
                && last.chars.len() < 128
            {
                last.chars.end += 1;
                last.original.end = original.end;
                return;
            }
        }
        self.runs.push(CharRun {
            chars: index..index + 1,
            original,
        });
    }

    fn source_range(&self, token: Range<usize>) -> Option<Range<usize>> {
        let mut index = self
            .runs
            .partition_point(|run| run.chars.end <= token.start);
        let first = self.runs.get(index)?;
        if first.chars.start > token.start {
            return None;
        }
        let start = first.original.start
            + self.widths[first.chars.start..token.start]
                .iter()
                .map(|width| usize::from(*width))
                .sum::<usize>();
        let mut cursor = token.start;
        let mut end = start;
        while cursor < token.end {
            let run = self.runs.get(index)?;
            if run.chars.start > cursor {
                return None;
            }
            let original = run.original.start
                + self.widths[run.chars.start..cursor]
                    .iter()
                    .map(|width| usize::from(*width))
                    .sum::<usize>();
            if original != end {
                return None;
            }
            let next = token.end.min(run.chars.end);
            end += self.widths[cursor..next]
                .iter()
                .map(|width| usize::from(*width))
                .sum::<usize>();
            cursor = next;
            index += 1;
        }
        Some(start..end)
    }
}

pub(super) struct DestinationTracker {
    source_address: usize,
    source_len: usize,
    line_endings: Vec<Range<usize>>,
    text: BTreeMap<usize, TextMap>,
    // A parser scope may register the same allocation more than once. Keep
    // the displaced map so unwinding restores its caller's source identity.
    text_order: Vec<(usize, Option<TextMap>)>,
    chars: Vec<CharMap>,
    spans: Vec<SourceDestination>,
    labels: Option<Vec<ReferenceLabelSpan>>,
    html: bool,
    /// Present only for the metadata-only transclusion fence query.
    container_fences: Option<Vec<Range<usize>>>,
}

impl DestinationTracker {
    pub(super) fn new(source: &str) -> Self {
        Self {
            source_address: source.as_ptr() as usize,
            source_len: source.len(),
            line_endings: {
                let mut offset = 0;
                logical_lines_inclusive(source)
                    .filter_map(|line| {
                        let start = offset + logical_line_content(line).len();
                        offset += line.len();
                        (start < offset).then_some(start..offset)
                    })
                    .collect()
            },
            text: BTreeMap::new(),
            text_order: Vec::new(),
            chars: Vec::new(),
            spans: Vec::new(),
            labels: None,
            html: false,
            container_fences: None,
        }
    }

    /// Map the first and last literal characters while the container's copied
    /// suffixes are still registered. The envelope includes original line
    /// prefixes/newlines; callers use it to classify whole source lines, never
    /// claim that stripped Markdown markers are rendered code characters.
    pub(super) fn container_fence(&mut self, lines: &[&str]) {
        if self.container_fences.is_none() {
            return;
        }
        let first = lines.iter().find_map(|line| {
            let text = line.trim();
            let width = text.chars().next()?.len_utf8();
            self.text_range(&text[..width])
        });
        let last = lines.iter().rev().find_map(|line| {
            let text = line.trim();
            let width = text.chars().next_back()?.len_utf8();
            self.text_range(&text[text.len() - width..])
        });
        if let (Some(first), Some(last), Some(ranges)) = (first, last, &mut self.container_fences) {
            ranges.push(first.start..last.end);
        }
    }

    pub(super) fn checkpoint(&self) -> usize {
        self.text_order.len()
    }

    pub(super) fn restore(&mut self, checkpoint: usize) {
        for (address, previous) in self.text_order.drain(checkpoint..).rev() {
            if let Some(map) = previous {
                self.text.insert(address, map);
            } else {
                self.text.remove(&address);
            }
        }
    }

    pub(super) fn text_range(&self, text: &str) -> Option<Range<usize>> {
        let address = text.as_ptr() as usize;
        if let Some(offset) = address.checked_sub(self.source_address)
            && text.len() <= self.source_len.saturating_sub(offset)
            && offset <= self.source_len
        {
            return Some(offset..offset + text.len());
        }
        let (&base, map) = self.text.range(..=address).next_back()?;
        let offset = address.checked_sub(base)?;
        if offset > map.len || text.len() > map.len - offset {
            return None;
        }
        let end = offset + text.len();
        let index = map.runs.partition_point(|run| run.copied.end <= offset);
        let run = map.runs.get(index)?;
        if run.copied.start > offset || run.copied.end < end {
            return None;
        }
        if run.original.len() != run.copied.len() {
            return Some(run.original.clone());
        }
        let start = run.original.start + offset - run.copied.start;
        Some(start..start + text.len())
    }

    fn push_run(runs: &mut Vec<TextRun>, copied: Range<usize>, original: Range<usize>) {
        if copied.is_empty() {
            return;
        }
        if let Some(last) = runs.last_mut()
            && last.copied.end == copied.start
            && last.original.end == original.start
            && last.copied.len() == last.original.len()
            && copied.len() == original.len()
        {
            last.copied.end = copied.end;
            last.original.end = original.end;
            return;
        }
        runs.push(TextRun { copied, original });
    }

    /// Register exact copied pieces. Inserted separators have no source bytes.
    pub(super) fn map_parts(&mut self, output: &str, parts: &[(usize, &str)]) {
        let mut runs = Vec::new();
        for &(offset, part) in parts {
            for (index, character) in part.char_indices() {
                let end = index + character.len_utf8();
                if let Some(original) = self.text_range(&part[index..end]) {
                    Self::push_run(&mut runs, offset + index..offset + end, original);
                }
            }
        }
        if !output.is_empty() {
            let address = output.as_ptr() as usize;
            // An entirely synthesized replacement must shadow an older map
            // too; otherwise reused storage can acquire an invented origin.
            let previous = self.text.insert(
                address,
                TextMap {
                    len: output.len(),
                    runs,
                },
            );
            self.text_order.push((address, previous));
        }
    }

    /// Container helpers only alter a prefix (markers, task boxes, or tabs).
    /// Preserve the exact shared suffix; synthesized prefix characters are not
    /// assigned invented source positions.
    pub(super) fn map_suffix(&mut self, output: &str, original: &str) {
        let mut common = output
            .bytes()
            .rev()
            .zip(original.bytes().rev())
            .take_while(|(left, right)| left == right)
            .count();
        while common > 0
            && (!output.is_char_boundary(output.len() - common)
                || !original.is_char_boundary(original.len() - common))
        {
            common -= 1;
        }
        if common > 0 {
            self.map_parts(
                output,
                &[(output.len() - common, &original[original.len() - common..])],
            );
        } else {
            self.map_parts(output, &[]);
        }
        self.map_generated_prefix(output, original, common);
    }

    pub(super) fn line_ending(&self, text: &str) -> Option<Range<usize>> {
        let width = text.chars().next_back()?.len_utf8();
        let end = self.text_range(&text[text.len() - width..])?.end;
        self.line_endings
            .get(self.line_endings.partition_point(|range| range.start < end))
            .filter(|range| range.start == end)
            .cloned()
    }

    fn append_chars(&self, map: &mut CharMap, text: &str) {
        let mut source = text.char_indices().peekable();
        while let Some((index, character)) = source.next() {
            // The inline buffer normalizes a CRLF pair to one '\n'. Its one
            // character still covers both original bytes in editable ranges.
            let mut width = character.len_utf8();
            if character == '\r' && source.peek().is_some_and(|(_, ch)| *ch == '\n') {
                let _ = source.next();
                width += 1;
            }
            map.push(width, self.text_range(&text[index..index + width]));
        }
    }

    pub(super) fn push_chars_text(&mut self, chars: &[char], text: &str) {
        let mut map = CharMap {
            address: chars.as_ptr() as usize,
            widths: Vec::with_capacity(chars.len()),
            runs: Vec::new(),
        };
        self.append_chars(&mut map, text);
        self.chars.push(map);
    }

    pub(super) fn push_chars_lines(&mut self, chars: &[char], lines: &[&str]) {
        let mut map = CharMap {
            address: chars.as_ptr() as usize,
            widths: Vec::with_capacity(chars.len()),
            runs: Vec::new(),
        };
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                let ending = self.line_ending(lines[index - 1]);
                map.push(ending.as_ref().map_or(1, Range::len), ending);
            }
            let text = line.trim_start_matches([' ', '\t']);
            let text = if index + 1 == lines.len() {
                text.trim_end_matches([' ', '\t'])
            } else {
                text
            };
            self.append_chars(&mut map, text);
        }
        self.chars.push(map);
    }

    pub(super) fn pop_chars(&mut self) {
        self.chars.pop();
    }

    pub(super) fn record(&mut self, chars: &[char], token: Range<usize>, dest: &str) {
        self.record_token(chars, token, dest, DestinationSyntax::Markdown);
    }

    fn record_token(
        &mut self,
        chars: &[char],
        token: Range<usize>,
        dest: &str,
        syntax: DestinationSyntax,
    ) -> bool {
        if token.is_empty() || dest.is_empty() || (!self.html && self.labels.is_none()) {
            return false;
        }
        let Some(range) = self.char_range(chars, token) else {
            return false;
        };
        self.spans.push(SourceDestination {
            token: DestinationSpan {
                range,
                dest: dest.to_string(),
            },
            syntax,
        });
        true
    }

    pub(super) fn char_range(&self, chars: &[char], token: Range<usize>) -> Option<Range<usize>> {
        if token.start > token.end || token.end > chars.len() {
            return None;
        }
        let address = chars.as_ptr() as usize;
        let char_size = std::mem::size_of::<char>();
        let (map, offset) = self.chars.iter().rev().find_map(|map| {
            let bytes = address.checked_sub(map.address)?;
            if bytes % char_size != 0 {
                return None;
            }
            let offset = bytes / char_size;
            (offset <= map.widths.len() && chars.len() <= map.widths.len() - offset)
                .then_some((map, offset))
        })?;
        map.source_range(offset + token.start..offset + token.end)
    }

    /// Called only after a reference use survives speculative link parsing.
    pub(super) fn reference_use(&mut self, chars: &[char], open: usize, close: usize, next: usize) {
        if self.labels.is_none() {
            return;
        }
        let (form, token) = if next == close + 1 {
            (ReferenceLabelForm::Shortcut, open + 1..close)
        } else if next == close + 3 {
            (ReferenceLabelForm::Collapsed, open + 1..close)
        } else {
            (ReferenceLabelForm::Full, close + 2..next - 1)
        };
        let Some(key) = normalize_reference_label_chars(&chars[token.clone()]) else {
            return;
        };
        let label = self.char_range(chars, token);
        let replacement = match form {
            ReferenceLabelForm::Full => label.clone(),
            ReferenceLabelForm::Collapsed => self.char_range(chars, close + 1..next),
            ReferenceLabelForm::Shortcut => self
                .char_range(chars, close..close + 1)
                .map(|range| range.end..range.end),
            ReferenceLabelForm::Definition => None,
        };
        if let Some(labels) = &mut self.labels {
            labels.push(ReferenceLabelSpan {
                key,
                form,
                label,
                replacement,
            });
        }
    }

    /// Invoked only after the Markdown parser accepts a real HTML node. Code,
    /// escaped tags, titles and failed speculative parses cannot supply URLs.
    pub(super) fn inline_html(&mut self, chars: &[char], start: usize, html: &str) {
        if !self.html {
            return;
        }
        for span in super::safe_html::url_spans(html) {
            let begin = start + html[..span.range.start].chars().count();
            let end = begin + html[span.range.clone()].chars().count();
            if !self.record_token(
                chars,
                begin..end,
                &span.destination,
                DestinationSyntax::HtmlAttribute,
            ) && let Some(content) = span.quoted_content_range(html)
            {
                let begin = start + html[..content.start].chars().count();
                let end = begin + html[content].chars().count();
                self.record_token(
                    chars,
                    begin..end,
                    &span.destination,
                    DestinationSyntax::HtmlAttributeContent,
                );
            }
        }
    }

    pub(super) fn block_html(&mut self, html: &str, lines: &[&str]) {
        if !self.html {
            return;
        }
        let mut offset = 0;
        let parts: Vec<_> = lines
            .iter()
            .map(|line| {
                let part = (offset, *line);
                offset += line.len() + 1;
                part
            })
            .collect();
        let checkpoint = self.checkpoint();
        self.map_parts(html, &parts);
        for span in super::safe_html::url_spans(html) {
            let mapped = self
                .text_range(&html[span.range.clone()])
                .map(|range| (range, DestinationSyntax::HtmlAttribute))
                .or_else(|| {
                    let content = span.quoted_content_range(html)?;
                    Some((
                        self.text_range(&html[content])?,
                        DestinationSyntax::HtmlAttributeContent,
                    ))
                });
            if let Some((range, syntax)) = mapped {
                self.spans.push(SourceDestination {
                    token: DestinationSpan {
                        range,
                        dest: span.destination,
                    },
                    syntax,
                });
            }
        }
        self.restore(checkpoint);
    }

    /// Invoked only after the real paragraph-aware collector accepts a def.
    pub(super) fn reference(&mut self, lines: &[&str], key: &str, dest: &str) {
        if self.container_fences.is_some() {
            return;
        }
        let joined = lines.join("\n");
        let mut offset = 0;
        let parts: Vec<_> = lines
            .iter()
            .map(|line| {
                let part = (offset, *line);
                offset += line.len() + 1;
                part
            })
            .collect();
        let checkpoint = self.checkpoint();
        self.map_parts(&joined, &parts);
        let chars: Vec<_> = joined.chars().collect();
        self.push_chars_text(&chars, &joined);
        if self.labels.is_some() {
            let mut open = 0;
            skip_spaces(&chars, &mut open);
            let label = find_closing_bracket(&chars, open)
                .and_then(|close| self.char_range(&chars, open + 1..close));
            if let Some(labels) = &mut self.labels {
                labels.push(ReferenceLabelSpan {
                    key: key.to_string(),
                    form: ReferenceLabelForm::Definition,
                    replacement: label.clone(),
                    label,
                });
            }
        }
        let token = (|| {
            let mut index = 0;
            skip_spaces(&chars, &mut index);
            let close = find_closing_bracket(&chars, index)?;
            if chars.get(close + 1) != Some(&':') {
                return None;
            }
            index = close + 2;
            skip_spaces_or_newline(&chars, &mut index);
            let start = index;
            // The parser's ASCII reference fast path deliberately accepts a
            // complete non-whitespace bare token, including unmatched parens.
            if lines
                .first()
                .and_then(|line| parse_simple_ascii_reference_definition(line))
                .is_some_and(|(_, reference)| reference.dest == dest)
            {
                if chars.get(index) == Some(&'<') {
                    index += 1;
                    while chars.get(index).is_some_and(|character| *character != '>') {
                        index += 1;
                    }
                    if chars.get(index) != Some(&'>') {
                        return None;
                    }
                    index += 1;
                } else {
                    while chars
                        .get(index)
                        .is_some_and(|character| !character.is_ascii_whitespace())
                    {
                        index += 1;
                    }
                }
            } else {
                if parse_link_destination(&chars, &mut index)?.as_str() != dest {
                    return None;
                }
            }
            Some(start..index)
        })();
        if let Some(token) = token {
            self.record(&chars, token, dest);
        }
        self.pop_chars();
        self.restore(checkpoint);
    }
}

impl ParseProfiler {
    pub(super) fn destination_checkpoint(&self) -> usize {
        self.destinations
            .as_ref()
            .map_or(0, DestinationTracker::checkpoint)
    }

    pub(super) fn destination_restore(&mut self, checkpoint: usize) {
        if let Some(tracker) = &mut self.destinations {
            tracker.restore(checkpoint);
        }
    }

    pub(super) fn destination_suffix(&mut self, output: &str, original: &str) {
        if let Some(tracker) = &mut self.destinations {
            tracker.map_suffix(output, original);
        }
    }

    pub(super) fn destination_parts(&mut self, output: &str, parts: &[(usize, &str)]) {
        if let Some(tracker) = &mut self.destinations {
            tracker.map_parts(output, parts);
        }
    }

    pub(super) fn destination_mark(&self) -> (usize, usize) {
        self.destinations.as_ref().map_or((0, 0), |tracker| {
            (
                tracker.spans.len(),
                tracker.labels.as_ref().map_or(0, Vec::len),
            )
        })
    }

    pub(super) fn destination_rollback(&mut self, mark: (usize, usize)) {
        if let Some(tracker) = &mut self.destinations {
            tracker.spans.truncate(mark.0);
            if let Some(labels) = &mut tracker.labels {
                labels.truncate(mark.1);
            }
        }
    }

    /// Image alt text flattens its inline nodes, so their URLs are not emitted
    /// destinations. Accepted reference labels still determine that text and
    /// need coordinated source edits when their definitions are renamed.
    pub(super) fn destination_tokens_rollback(&mut self, mark: (usize, usize)) {
        if let Some(tracker) = &mut self.destinations {
            tracker.spans.truncate(mark.0);
        }
    }
}
