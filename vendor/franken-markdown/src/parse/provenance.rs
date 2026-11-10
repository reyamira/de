//! Source identities captured by the native parser, before layout copies text.
use super::*;
use std::cell::RefCell;
#[path = "provenance_transform.rs"]
mod transform;
pub(super) use transform::{
    clone_block, clone_definitions, clone_inline, clone_list, clone_table, soft_break,
};
pub(crate) use transform::{image_fallback, prepare_endnotes, with_assets};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Origin {
    pub bytes: Range<usize>,
    pub source: Range<usize>,
}

#[derive(Default, Debug)]
pub(crate) struct Provenance {
    pub leaves: BTreeMap<usize, (usize, Vec<Origin>)>,
}

thread_local! {
    static OUTPUT: RefCell<Provenance> = RefCell::new(Provenance::default());
}

pub(super) fn register(text: &str, origins: Vec<Origin>) {
    if !text.is_empty() {
        OUTPUT.with(|output| {
            let mut output = output.borrow_mut();
            let base = text.as_ptr() as usize;
            let end = base + text.len();
            while let Some((&address, _)) = output.leaves.range(base..end).next() {
                output.leaves.remove(&address);
            }
            if let Some((&address, (len, _))) = output.leaves.range(..base).next_back()
                && address + *len > base
            {
                output.leaves.remove(&address);
            }
            output.leaves.insert(base, (text.len(), origins));
        });
    }
}

pub(super) fn copy(text: &str) -> String {
    let result = text.to_owned();
    let origins = OUTPUT.with(|output| output.borrow().get(text));
    register(&result, origins);
    result
}

pub(super) fn append(text: &mut String, suffix: &str) {
    let mut origins = OUTPUT.with(|output| {
        output
            .borrow_mut()
            .leaves
            .remove(&(text.as_ptr() as usize))
            .map_or_else(Vec::new, |(_, origins)| origins)
    });
    let offset = text.len();
    let more = OUTPUT.with(|output| output.borrow().get(suffix));
    origins.extend(more.into_iter().map(|mut origin| {
        origin.bytes.start += offset;
        origin.bytes.end += offset;
        origin
    }));
    text.push_str(suffix);
    register(text, origins);
}

impl Provenance {
    pub(crate) fn get(&self, text: &str) -> Vec<Origin> {
        let address = text.as_ptr() as usize;
        let Some((&base, (length, origins))) = self.leaves.range(..=address).next_back() else {
            return Vec::new();
        };
        let offset = address - base;
        let Some(end) = offset.checked_add(text.len()) else {
            return Vec::new();
        };
        if offset > *length || end > *length {
            return Vec::new();
        }
        origins
            .iter()
            .filter_map(|origin| {
                let start = origin.bytes.start.max(offset);
                let stop = origin.bytes.end.min(end);
                (start < stop).then(|| Origin {
                    bytes: start - offset..stop - offset,
                    source: origin.source.clone(),
                })
            })
            .collect()
    }
}

pub(crate) fn parse(source: &str) -> (Document, Provenance) {
    OUTPUT.with(|output| *output.borrow_mut() = Provenance::default());
    let mut profiler = ParseProfiler::disabled();
    profiler.destinations = Some(destinations::DestinationTracker::new(source));
    let document = parse_document_inner(source, &mut profiler);
    let origins = OUTPUT.with(|output| std::mem::take(&mut *output.borrow_mut()));
    (document, origins)
}

pub(super) fn text_origins(text: &str, profiler: &ParseProfiler) -> Vec<Origin> {
    text.char_indices()
        .filter_map(|(start, ch)| {
            let end = start + ch.len_utf8();
            Some(Origin {
                bytes: start..end,
                source: profiler
                    .destinations
                    .as_ref()?
                    .text_range(&text[start..end])?,
            })
        })
        .collect()
}

pub(super) fn char_origins(output: &str, chars: &[char], profiler: &ParseProfiler) -> Vec<Origin> {
    let Some(tracker) = &profiler.destinations else {
        return Vec::new();
    };
    let mut offset = 0;
    chars
        .iter()
        .enumerate()
        .filter_map(|(index, ch)| {
            let start = offset;
            offset += ch.len_utf8();
            Some(Origin {
                bytes: start..offset.min(output.len()),
                source: tracker.char_range(chars, index..index + 1)?,
            })
        })
        .filter(|origin| !origin.bytes.is_empty())
        .collect()
}

pub(super) fn origins_for_chars(chars: &[char], profiler: &ParseProfiler) -> Vec<Origin> {
    let Some(tracker) = &profiler.destinations else {
        return Vec::new();
    };
    let mut offset = 0;
    chars
        .iter()
        .enumerate()
        .filter_map(|(index, ch)| {
            let start = offset;
            offset += ch.len_utf8();
            Some(Origin {
                bytes: start..offset,
                source: tracker.char_range(chars, index..index + 1)?,
            })
        })
        .collect()
}

pub(super) fn synthetic(text: String) -> String {
    register(&text, Vec::new());
    text
}

pub(super) fn visible_footnote(id: &str) -> String {
    let originals = OUTPUT.with(|output| output.borrow().get(id));
    let mut text = String::from("[^");
    register(&text, Vec::new());
    append(&mut text, id);
    let mut origins = OUTPUT.with(|output| output.borrow().get(&text));
    if let Some(first) = originals.first()
        && first.source.start >= 2
    {
        origins.push(Origin {
            bytes: 0..2,
            source: first.source.start - 2..first.source.start,
        });
    }
    if let Some(last) = originals.last() {
        origins.push(Origin {
            bytes: text.len()..text.len() + 1,
            source: last.source.end..last.source.end + 1,
        });
    }
    text.push(']');
    origins.sort_unstable_by_key(|origin| origin.bytes.start);
    register(&text, origins);
    text
}

#[derive(Default)]
pub(super) struct Buffer {
    text: String,
    origins: Vec<Origin>,
}

impl Buffer {
    pub fn len(&self) -> usize {
        self.text.len()
    }
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
    pub fn push(&mut self, ch: char) {
        self.text.push(ch);
    }
    pub fn push_str(&mut self, text: &str) {
        self.text.push_str(text);
    }
    pub fn pop(&mut self) {
        self.text.pop();
        let len = self.text.len();
        self.origins.retain(|origin| origin.bytes.start < len);
        if let Some(last) = self.origins.last_mut() {
            last.bytes.end = last.bytes.end.min(len);
        }
    }
    pub fn record(
        &mut self,
        previous: usize,
        chars: &[char],
        consumed: Range<usize>,
        profiler: &ParseProfiler,
    ) {
        if self.text.len() <= previous {
            return;
        }
        let Some(tracker) = &profiler.destinations else {
            return;
        };
        let appended = &self.text[previous..];
        let literal = appended.chars().eq(chars[consumed.clone()].iter().copied());
        if literal {
            let mut offset = previous;
            for index in consumed {
                let end = offset + chars[index].len_utf8();
                if let Some(source) = tracker.char_range(chars, index..index + 1) {
                    self.origins.push(Origin {
                        bytes: offset..end,
                        source,
                    });
                }
                offset = end;
            }
        } else {
            // Entities and escapes can consume several source scalars for one
            // visible cluster. Preserve each disjoint raw source span.
            for index in consumed {
                if let Some(source) = tracker.char_range(chars, index..index + 1) {
                    self.origins.push(Origin {
                        bytes: previous..self.text.len(),
                        source,
                    });
                }
            }
        }
    }
    pub fn finish(self) -> String {
        register(&self.text, self.origins);
        self.text
    }
}
