//! Case-insensitive literal matches with original UTF-8 byte ranges.

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
