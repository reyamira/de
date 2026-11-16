//! Case-insensitive literal and fuzzy name matches with original UTF-8 byte ranges.

use std::ops::Range;

/// Uses Unicode lowercase, not full case folding or regular expressions.
#[derive(Clone, Debug)]
pub(crate) struct LiteralMatcher {
    query: String,
}

impl LiteralMatcher {
    pub(crate) fn new(query: &str) -> Self {
        Self {
            query: query.to_lowercase(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    /// Returns ordered, non-overlapping ranges in the original text.
    ///
    /// A match within a lowercase expansion selects the whole source character.
    /// When separate folded matches touch the same source character, keep the
    /// first match only. Empty queries have no matches.
    pub(crate) fn ranges(&self, text: &str) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        self.visit(text, |range| {
            ranges.push(range);
            true
        });
        ranges
    }

    /// Finds the first original-byte match without collecting later matches.
    pub(crate) fn first(&self, text: &str) -> Option<Range<usize>> {
        let mut first = None;
        self.visit(text, |range| {
            first = Some(range);
            false
        });
        first
    }

    fn visit(&self, text: &str, mut visit: impl FnMut(Range<usize>) -> bool) {
        if self.is_empty() {
            return;
        }
        // Lowercase the entire string: unlike char::to_lowercase, this preserves
        // context-sensitive final sigma, as the previous search policy did.
        let folded = text.to_lowercase();
        if text.is_ascii() {
            for (start, matched) in folded.match_indices(self.query.as_str()) {
                if !visit(start..start + matched.len()) {
                    break;
                }
            }
            return;
        }
        let mut positions = text.char_indices().scan(0, |folded_start, (start, ch)| {
            // Context-sensitive sigma changes its value, but not its UTF-8
            // length. Other lowercase mappings use these per-character lengths.
            let folded_len = ch.to_lowercase().map(char::len_utf8).sum::<usize>();
            let folded_range = *folded_start..*folded_start + folded_len;
            *folded_start += folded_len;
            Some((folded_range, start..start + ch.len_utf8()))
        });
        let mut position = positions.next();
        let mut previous_end = 0;
        for (start, matched) in folded.match_indices(self.query.as_str()) {
            while position
                .as_ref()
                .is_some_and(|(range, _)| range.end <= start)
            {
                position = positions.next();
            }
            let source_start = position
                .as_ref()
                .expect("folded match source start")
                .1
                .start;
            let end = start + matched.len();
            while position.as_ref().is_some_and(|(range, _)| range.end < end) {
                position = positions.next();
            }
            let source_end = position.as_ref().expect("folded match source end").1.end;
            if previous_end <= source_start {
                previous_end = source_end;
                if !visit(source_start..source_end) {
                    break;
                }
            }
        }
    }
}

/// Lower values sort first: exact, substring, then ordered subsequence matches.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct NameRank {
    kind: u8,
    in_directory: bool,
    gaps: usize,
    start: usize,
    length: usize,
}

pub(crate) struct NameMatch {
    pub(crate) rank: NameRank,
    pub(crate) ranges: Vec<Range<usize>>,
}

/// Names use the same Unicode lowercase policy as literal content search.
pub(crate) struct NameMatcher {
    query: String,
    folded: String,
    scratch: Vec<Range<usize>>,
}

impl NameMatcher {
    pub(crate) fn new(query: &str) -> Self {
        Self {
            query: query.to_lowercase(),
            folded: String::new(),
            scratch: Vec::new(),
        }
    }

    fn match_name(&mut self, text: &str) -> Option<(NameRank, usize)> {
        if self.query.is_empty() {
            return None;
        }
        self.folded.clear();
        if text.is_ascii() {
            self.folded
                .extend(text.bytes().map(|byte| byte.to_ascii_lowercase() as char));
        } else {
            self.folded = text.to_lowercase();
        }
        let folded = self.folded.as_str();
        let basename = folded.rfind('/').map_or(0, |byte| byte + 1);
        let leaf = &folded[basename..];
        // Prefer basename matches within each class, but never let a fuzzy
        // basename displace an exact or substring match of the whole path.
        self.scratch.clear();
        let contiguous = if leaf == self.query {
            Some((0, basename, 0..leaf.len()))
        } else if folded == self.query {
            Some((0, 0, 0..folded.len()))
        } else if let Some(start) = leaf.find(&self.query) {
            Some((1, basename, start..start + self.query.len()))
        } else {
            folded
                .find(&self.query)
                .map(|start| (1, 0, start..start + self.query.len()))
        };
        let (kind, offset) = if let Some((kind, offset, range)) = contiguous {
            self.scratch.push(range);
            (kind, offset)
        } else if Self::subsequence(&self.query, leaf, &mut self.scratch) {
            (2, basename)
        } else if Self::subsequence(&self.query, folded, &mut self.scratch) {
            (2, 0)
        } else {
            return None;
        };
        let start = self.scratch.first()?.start;
        let end = self.scratch.last()?.end;
        let gaps = end - start - self.scratch.iter().map(|range| range.len()).sum::<usize>();
        let rank = NameRank {
            kind,
            in_directory: offset == 0 && start < basename,
            gaps,
            start,
            length: text.len(),
        };
        Some((rank, offset))
    }

    pub(crate) fn rank(&mut self, text: &str) -> Option<NameRank> {
        self.match_name(text).map(|(rank, _)| rank)
    }

    pub(crate) fn find(&mut self, text: &str) -> Option<NameMatch> {
        let (rank, offset) = self.match_name(text)?;
        if text.is_ascii() {
            return Some(NameMatch {
                rank,
                ranges: self
                    .scratch
                    .iter()
                    .map(|range| range.start + offset..range.end + offset)
                    .collect(),
            });
        }
        let mut ranges: Vec<Range<usize>> = Vec::with_capacity(self.scratch.len());
        // A lowercase expansion can match more than once inside one original
        // character. Merge those matches, and preserve the original UTF-8 bytes.
        let mut positions = text.char_indices().scan(0, |folded_start, (byte, ch)| {
            let length = ch.to_lowercase().map(char::len_utf8).sum::<usize>();
            let range = *folded_start..*folded_start + length;
            *folded_start += length;
            Some((range, byte..byte + ch.len_utf8()))
        });
        let mut position = positions.next();
        for matched in &self.scratch {
            let matched = matched.start + offset..matched.end + offset;
            while position.as_ref()?.0.end <= matched.start {
                position = positions.next();
            }
            let source_start = position.as_ref()?.1.start;
            while position.as_ref()?.0.end < matched.end {
                position = positions.next();
            }
            let source_end = position.as_ref()?.1.end;
            if let Some(previous) = ranges.last_mut().filter(|range| range.end >= source_start) {
                previous.end = previous.end.max(source_end);
            } else {
                ranges.push(source_start..source_end);
            }
        }
        Some(NameMatch { rank, ranges })
    }

    fn subsequence(query: &str, text: &str, ranges: &mut Vec<Range<usize>>) -> bool {
        ranges.clear();
        let mut characters = text.char_indices();
        for query in query.chars() {
            let Some((byte, ch)) = characters.find(|(_, ch)| *ch == query) else {
                return false;
            };
            if let Some(previous) = ranges.last_mut().filter(|range| range.end == byte) {
                previous.end += ch.len_utf8();
            } else {
                ranges.push(byte..byte + ch.len_utf8());
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_ranges_keep_original_utf8_bytes_and_every_occurrence() {
        let text = "β [A.*] café [a.*]";
        let ranges = LiteralMatcher::new("[a.*]").ranges(text);
        assert_eq!(ranges, [3..8, 15..20]);
        assert_eq!(&text[ranges[0].clone()], "[A.*]");
        assert_eq!(&text[ranges[1].clone()], "[a.*]");
        assert_eq!(
            LiteralMatcher::new("É").ranges(text),
            [Range { start: 12, end: 14 }]
        );
        assert!(LiteralMatcher::new("").ranges(text).is_empty());
        assert!(LiteralMatcher::new("absent").ranges(text).is_empty());
    }

    #[test]
    fn expansion_matches_select_whole_source_characters() {
        let text = "aİb i\u{307} İ";
        assert_eq!(LiteralMatcher::new("i").ranges(text), [1..3, 5..6, 9..11]);
        assert_eq!(
            LiteralMatcher::new("\u{307}").ranges(text),
            [1..3, 6..8, 9..11]
        );
        assert_eq!(LiteralMatcher::new("İ").ranges(text), [1..3, 5..8, 9..11]);
        assert_eq!(LiteralMatcher::new("k").ranges("KKk"), [0..3, 3..4, 4..5]);
    }

    #[test]
    fn whole_string_lowercase_preserves_contextual_sigma() {
        assert_eq!(LiteralMatcher::new("ΟΣ").ranges("ΟΣ οσ ος"), [0..4, 10..14]);
        assert_eq!(LiteralMatcher::new("σ").ranges("ΟΣ ΣΑ Σ"), [5..7, 10..12]);
        assert!(LiteralMatcher::new("ss").ranges("ß").is_empty());
    }

    #[test]
    fn nonoverlapping_occurrences_use_original_character_boundaries() {
        assert_eq!(LiteralMatcher::new("aa").ranges("aaaaa"), [0..2, 2..4]);
        assert_eq!(
            LiteralMatcher::new("\u{307}i").ranges("İİİ"),
            [Range { start: 0, end: 4 }]
        );
        assert_eq!(LiteralMatcher::new("\u{307}").ranges("İİ"), [0..2, 2..4]);
    }

    #[test]
    fn first_matches_the_shared_unicode_and_literal_policy() {
        for (query, text, expected) in [
            ("", "needle", None),
            ("absent", "needle", None),
            ("nEeDlE", "β needle NEEDLE", Some(3..9)),
            ("\u{307}i", "İİİ", Some(0..4)),
            ("k", "β K K", Some(3..6)),
            ("ΟΣ", "οσ ΟΣ ος", Some(5..9)),
        ] {
            let matcher = LiteralMatcher::new(query);
            assert_eq!(matcher.first(text), expected);
            assert_eq!(matcher.first(text), matcher.ranges(text).into_iter().next());
        }
        let dense = "i".repeat(1024 * 1024);
        assert_eq!(LiteralMatcher::new("i").first(&dense), Some(0..1));
    }
}
