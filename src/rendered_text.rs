//! Native reading text and shaped-cluster geometry in final full-page pixels.
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Debug, Default)]
pub struct TextAnnotations {
    pub selection: Option<Range<usize>>,
    pub matches: Arc<[Range<usize>]>,
    pub active: Option<Range<usize>>,
}

#[derive(Debug)]
struct Cluster {
    bytes: Range<usize>,
    rect: TextRect,
    prefix_end: usize,
}

#[derive(Debug)]
struct SourceLink {
    source: Range<usize>,
    text: Range<usize>,
}

#[derive(Default, Debug)]
pub struct RenderedText {
    text: String,
    clusters: Vec<Cluster>,
    sources: Vec<SourceLink>,
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

impl RenderedText {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn hit_test(&self, x: f32, y: f32) -> Option<usize> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let cluster = self.clusters.iter().min_by(|a, b| {
            let distance = |rect: TextRect| {
                let dy = (rect.y - y).max(0.0).max(y - rect.y - rect.height);
                let dx = (rect.x - x).max(0.0).max(x - rect.x - rect.width);
                // A row's vertical distance takes priority over horizontal gaps.
                (dy, dx)
            };
            let da = distance(a.rect);
            let db = distance(b.rect);
            da.0.total_cmp(&db.0).then_with(|| da.1.total_cmp(&db.1))
        })?;
        Some(if x < cluster.rect.x + cluster.rect.width * 0.5 {
            cluster.bytes.start
        } else {
            cluster.bytes.end
        })
    }

    pub fn rectangles(&self, range: &Range<usize>) -> Vec<TextRect> {
        if range.is_empty() {
            return Vec::new();
        }
        let first = self
            .clusters
            .partition_point(|cluster| cluster.prefix_end <= range.start);
        let end = self
            .clusters
            .partition_point(|cluster| cluster.bytes.start < range.end);
        let mut rectangles: Vec<_> = self.clusters[first..end]
            .iter()
            .filter(|cluster| overlaps(&cluster.bytes, range))
            .map(|cluster| cluster.rect)
            .collect();
        rectangles.sort_unstable_by(|a, b| a.y.total_cmp(&b.y).then_with(|| a.x.total_cmp(&b.x)));
        rectangles.dedup_by(|rect, previous| {
            if (previous.y - rect.y).abs() < 0.01
                && (previous.height - rect.height).abs() < 0.01
                && rect.x <= previous.x + previous.width + 0.1
            {
                previous.width =
                    (rect.x + rect.width).max(previous.x + previous.width) - previous.x;
                true
            } else {
                false
            }
        });
        rectangles
    }

    pub fn source_match(
        &self,
        source: Range<usize>,
        visible_candidates: &[Range<usize>],
    ) -> Option<Range<usize>> {
        if source.is_empty() {
            return None;
        }
        let mut relevant: Vec<_> = self
            .sources
            .iter()
            .filter(|link| overlaps(&link.source, &source))
            .collect();
        if relevant.is_empty() {
            return None;
        }
        relevant.sort_unstable_by_key(|link| link.source.start);
        visible_candidates
            .iter()
            .find(|candidate| {
                let mut found = false;
                let mut covered = source.start;
                // Endnotes can reorder source spans. Resolve identity once before
                // considering visible matches; repeated text never needs a rescan.
                for link in &relevant {
                    if !overlaps(&link.text, candidate) {
                        continue;
                    }
                    let span = &link.source;
                    found = true;
                    if span.start > covered {
                        return false;
                    }
                    covered = covered.max(span.end);
                }
                found && covered >= source.end
            })
            .cloned()
    }

    pub(crate) fn scale(&mut self, x: f32, y: f32) {
        for cluster in &mut self.clusters {
            cluster.rect.x *= x;
            cluster.rect.y *= y;
            cluster.rect.width *= x;
            cluster.rect.height *= y;
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct CopyRun {
    bytes: Range<usize>,
    logical: Range<usize>,
}

/// Explicit logical-byte provenance owned by a layout piece or word.
/// Moving, cloning, slicing, or dropping text never changes its identity.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TextMap {
    runs: Vec<CopyRun>,
}

impl TextMap {
    pub(crate) fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }
    pub(crate) fn end(&self) -> Option<usize> {
        self.runs.iter().map(|run| run.logical.end).max()
    }

    fn visit(&self, bytes: &Range<usize>, mut visit: impl FnMut(CopyRun)) {
        let first = self
            .runs
            .partition_point(|run| run.bytes.end <= bytes.start);
        for run in &self.runs[first..] {
            if run.bytes.start >= bytes.end {
                break;
            }
            let start = run.bytes.start.max(bytes.start);
            let end = run.bytes.end.min(bytes.end);
            if start >= end {
                continue;
            }
            let logical = if run.bytes.len() == run.logical.len() {
                run.logical.start + start - run.bytes.start
                    ..run.logical.start + end - run.bytes.start
            } else {
                run.logical.clone()
            };
            visit(CopyRun {
                bytes: start - bytes.start..end - bytes.start,
                logical,
            });
        }
    }

    pub(crate) fn slice(&self, bytes: Range<usize>) -> Self {
        let mut runs = Vec::new();
        self.visit(&bytes, |run| runs.push(run));
        Self { runs }
    }

    pub(crate) fn append(&mut self, suffix: &Self, offset: usize) {
        self.runs.extend(suffix.runs.iter().cloned().map(|mut run| {
            run.bytes.start += offset;
            run.bytes.end += offset;
            run
        }));
    }

    fn bounds(&self, bytes: Range<usize>) -> Option<Range<usize>> {
        let mut start = usize::MAX;
        let mut end = 0;
        self.visit(&bytes, |run| {
            start = start.min(run.logical.start);
            end = end.max(run.logical.end);
        });
        (start < end).then_some(start..end)
    }

    pub(crate) fn expanded_code(&self, source: &str) -> (String, Self) {
        let mut text = String::new();
        let mut runs = Vec::new();
        let mut column = 0;
        for (offset, ch) in source.char_indices() {
            let start = text.len();
            let count = if ch == '\t' { 4 - column % 4 } else { 1 };
            for _ in 0..count {
                text.push(if ch == '\t' { ' ' } else { ch });
            }
            self.visit(&(offset..offset + ch.len_utf8()), |run| {
                runs.push(CopyRun {
                    bytes: start..text.len(),
                    logical: run.logical,
                })
            });
            column += count;
        }
        (text, Self { runs })
    }
}

/// Only AST leaves use address identities. The document keeps those strings
/// alive throughout layout. Transient pieces and words own explicit TextMaps.
#[derive(Default)]
pub(crate) struct TextLayout {
    result: RenderedText,
    leaves: BTreeMap<usize, (usize, Range<usize>)>,
    boundaries: std::collections::BTreeSet<usize>,
    provenance: Option<crate::fmd_parse::Provenance>,
}

impl TextLayout {
    pub(crate) fn new(provenance: crate::fmd_parse::Provenance) -> Self {
        Self {
            provenance: Some(provenance),
            ..Self::default()
        }
    }

    pub(crate) fn separator(&mut self, separator: char) {
        if !self.result.text.is_empty() && !self.result.text.ends_with(separator) {
            self.result.text.push(separator);
        }
    }

    pub(crate) fn cell_separator(&mut self) {
        self.result.text.push('\t');
    }
    pub(crate) fn row_separator(&mut self) {
        self.result.text.push('\n');
    }

    pub(crate) fn boundary(&mut self, identity: usize, separator: char) {
        if self.boundaries.insert(identity) {
            self.separator(separator);
        }
    }

    pub(crate) fn leaf(&mut self, text: &str) {
        if text.is_empty() || self.leaves.contains_key(&(text.as_ptr() as usize)) {
            return;
        }
        let start = self.result.text.len();
        self.result.text.push_str(text);
        if let Some(provenance) = &self.provenance {
            self.result
                .sources
                .extend(provenance.get(text).into_iter().map(|origin| SourceLink {
                    source: origin.source,
                    text: start + origin.bytes.start..start + origin.bytes.end,
                }));
        }
        self.leaves.insert(
            text.as_ptr() as usize,
            (text.len(), start..start + text.len()),
        );
    }

    pub(crate) fn map(&self, text: &str) -> TextMap {
        let address = text.as_ptr() as usize;
        let Some((&base, (len, logical))) = self.leaves.range(..=address).next_back() else {
            return TextMap::default();
        };
        let offset = address - base;
        if offset > *len || text.len() > *len - offset {
            return TextMap::default();
        }
        TextMap {
            runs: vec![CopyRun {
                bytes: 0..text.len(),
                logical: logical.start + offset..logical.start + offset + text.len(),
            }],
        }
    }

    pub(crate) fn synthetic_leaf(&mut self, text: &str) -> TextMap {
        if text.is_empty() {
            return TextMap::default();
        }
        let start = self.result.text.len();
        self.result.text.push_str(text);
        TextMap {
            runs: vec![CopyRun {
                bytes: 0..text.len(),
                logical: start..start + text.len(),
            }],
        }
    }

    pub(crate) fn record(&mut self, logical: &TextMap, bytes: Range<usize>, rect: TextRect) {
        let Some(range) = logical.bounds(bytes) else {
            return;
        };
        if rect.width.is_finite() && rect.height.is_finite() {
            self.result.clusters.push(Cluster {
                bytes: range,
                rect,
                prefix_end: 0,
            });
        }
    }

    pub(crate) fn record_gap(&mut self, previous: usize, next: &TextMap, rect: TextRect) {
        let Some(right) = next.bounds(0..usize::MAX) else {
            return;
        };
        let range = previous..right.start;
        if range.start < range.end
            && self.result.text[range.clone()]
                .chars()
                .all(char::is_whitespace)
        {
            self.result.clusters.push(Cluster {
                bytes: range,
                rect,
                prefix_end: 0,
            });
        }
    }

    pub(crate) fn finish(mut self) -> RenderedText {
        self.result
            .clusters
            .sort_unstable_by_key(|cluster| (cluster.bytes.start, cluster.bytes.end));
        let mut end = 0;
        for cluster in &mut self.result.clusters {
            end = end.max(cluster.bytes.end);
            cluster.prefix_end = end;
        }
        self.result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_identity_does_not_choose_an_equal_earlier_label() {
        let mut text = RenderedText {
            text: "same same".into(),
            ..Default::default()
        };
        text.sources = vec![
            SourceLink {
                source: 2..6,
                text: 0..4,
            },
            SourceLink {
                source: 15..19,
                text: 5..9,
            },
        ];
        assert_eq!(text.source_match(15..19, &[0..4, 5..9]), Some(5..9));
        assert_eq!(text.source_match(10..14, &[0..4, 5..9]), None);
    }

    #[test]
    fn composed_clusters_keep_utf8_boundaries_and_midpoint_affinity() {
        let text = RenderedText {
            text: "e\u{301}x".into(),
            clusters: vec![
                Cluster {
                    bytes: 0..3,
                    rect: TextRect {
                        x: 10.0,
                        y: 20.0,
                        width: 12.0,
                        height: 15.0,
                    },
                    prefix_end: 3,
                },
                Cluster {
                    bytes: 3..4,
                    rect: TextRect {
                        x: 22.0,
                        y: 20.0,
                        width: 9.0,
                        height: 15.0,
                    },
                    prefix_end: 4,
                },
            ],
            sources: Vec::new(),
        };
        assert_eq!(text.hit_test(15.0, 25.0), Some(0));
        assert_eq!(text.hit_test(17.0, 25.0), Some(3));
        assert_eq!(text.hit_test(100.0, 25.0), Some(4));
        assert_eq!(text.rectangles(&(1..3))[0].width, 12.0);
    }
}
