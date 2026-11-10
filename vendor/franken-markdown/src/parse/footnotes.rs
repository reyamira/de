//! Lossless PDF footnote preparation over the shared document AST.
//!
//! The PDF layout engine consumes ordinary blocks and inline text. Resolve
//! references once, move complete note bodies to the end, and leave their
//! internal block structure intact. Note-reference cycles are graph edges,
//! not recursive expansion: every definition is emitted at most once.

use std::borrow::Cow;
use std::collections::BTreeMap;

use franken_markdown::ast::{Block, Document, Inline};
/// Prefix of the fragment a page-bottom note reference links to, and of the
/// id its prepared definition carries (`fmd-fn-3` for note 3).
pub(crate) const PAGE_NOTE_PREFIX: &str = "fmd-fn-";
pub(super) fn endnotes_for_pdf(doc: &Document, paged: bool) -> Cow<'_, Document> {
    let mut notes = Notes::default();
    notes.collect(&doc.blocks);
    if notes.definitions.is_empty() && !notes.has_reference {
        return Cow::Borrowed(doc);
    }
    // Already prepared for page notes: nothing references the labeled
    // definitions any more, and preparing again would renumber them.
    if paged
        && !notes.has_reference
        && notes
            .indices
            .keys()
            .all(|id| id.starts_with(PAGE_NOTE_PREFIX))
    {
        return Cow::Borrowed(doc);
    }

    let mut numbering = number_notes(doc, &notes);
    numbering.paged = paged;
    let mut blocks = rewrite_blocks(&doc.blocks, &notes, &numbering);
    if !notes.definitions.is_empty() && !paged {
        blocks.push(Block::Heading {
            level: 2,
            inlines: vec![Inline::Text(super::provenance::synthetic(
                "Notes".to_string(),
            ))],
        });
        for &index in &numbering.order {
            let mut body = rewrite_blocks(notes.definitions[index], &notes, &numbering);
            let label = superscript(numbering.numbers[index]);
            match body.first_mut() {
                Some(Block::Paragraph(inlines)) if super::toc::marker_depth(inlines).is_none() => {
                    inlines.insert(
                        0,
                        Inline::Text(super::provenance::synthetic(format!("{label} "))),
                    );
                }
                _ => {
                    // A code fence, table, list, image, equation, or heading
                    // is not a paragraph to flatten. Give it a separate note
                    // label and preserve the original blocks underneath it.
                    body.insert(0, Block::Paragraph(vec![Inline::Text(label)]));
                }
            }
            blocks.extend(body);
        }
    }
    if paged {
        for &index in &numbering.order {
            let number = numbering.numbers[index];
            let mut body = rewrite_blocks(notes.definitions[index], &notes, &numbering);
            let label = superscript(number);
            match body.first_mut() {
                Some(Block::Paragraph(inlines)) if super::toc::marker_depth(inlines).is_none() => {
                    inlines.insert(
                        0,
                        Inline::Text(super::provenance::synthetic(format!("{label} "))),
                    );
                }
                _ => body.insert(0, Block::Paragraph(vec![Inline::Text(label)])),
            }
            blocks.push(Block::FootnoteDefinition {
                id: format!("{PAGE_NOTE_PREFIX}{number}"),
                blocks: body,
            });
        }
    }
    Cow::Owned(Document { blocks })
}

fn number_notes(doc: &Document, notes: &Notes<'_>) -> Numbering {
    let mut numbering = Numbering {
        numbers: vec![0; notes.definitions.len()],
        order: Vec::with_capacity(notes.definitions.len()),
        paged: false,
    };
    references_in_blocks(&doc.blocks, &mut |id| numbering.reference(notes, id));
    let mut visited = 0;
    numbering.follow_references(notes, &mut visited);
    for index in 0..notes.definitions.len() {
        numbering.assign(index);
        numbering.follow_references(notes, &mut visited);
    }
    numbering
}
#[derive(Default)]
struct Notes<'a> {
    indices: BTreeMap<&'a str, usize>,
    definitions: Vec<&'a [Block]>,
    has_reference: bool,
}

impl<'a> Notes<'a> {
    fn collect(&mut self, blocks: &'a [Block]) {
        for block in blocks {
            match block {
                Block::FootnoteDefinition { id, blocks } => {
                    if !self.indices.contains_key(id.as_str()) {
                        let index = self.definitions.len();
                        self.indices.insert(id, index);
                        self.definitions.push(blocks);
                        self.collect(blocks);
                    }
                }
                Block::BlockQuote(inner) => self.collect(inner),
                Block::List(list) => {
                    for item in &list.items {
                        self.collect(&item.blocks);
                    }
                }
                _ => references_in_block(block, &mut |_| self.has_reference = true),
            }
        }
    }
}

struct Numbering {
    numbers: Vec<usize>,
    order: Vec<usize>,
    /// References link to their page-bottom note (see [`for_pdf_paged`]).
    paged: bool,
}

impl Numbering {
    fn assign(&mut self, index: usize) {
        if self.numbers[index] == 0 {
            self.numbers[index] = self.order.len() + 1;
            self.order.push(index);
        }
    }

    fn reference(&mut self, notes: &Notes<'_>, id: &str) {
        if let Some(&index) = notes.indices.get(id) {
            self.assign(index);
        }
    }

    fn follow_references(&mut self, notes: &Notes<'_>, visited: &mut usize) {
        while *visited < self.order.len() {
            let index = self.order[*visited];
            *visited += 1;
            references_in_blocks(notes.definitions[index], &mut |id| {
                self.reference(notes, id)
            });
        }
    }
}

/// Definitions do not appear in normal flow. Their references are traversed
/// separately when their turn arrives in the note queue.
fn references_in_blocks(blocks: &[Block], visit: &mut impl FnMut(&str)) {
    for block in blocks {
        references_in_block(block, visit);
    }
}

fn references_in_block(block: &Block, visit: &mut impl FnMut(&str)) {
    match block {
        Block::Paragraph(inlines) | Block::Heading { inlines, .. } => {
            references_in_inlines(inlines, visit);
        }
        Block::BlockQuote(inner) => references_in_blocks(inner, visit),
        Block::List(list) => {
            for item in &list.items {
                references_in_blocks(&item.blocks, visit);
            }
        }
        Block::Table(table) => {
            for cell in &table.head {
                references_in_inlines(cell, visit);
            }
            for row in &table.rows {
                for cell in row {
                    references_in_inlines(cell, visit);
                }
            }
        }
        Block::DefinitionList(items) => {
            for item in items {
                for inlines in item.terms.iter().chain(&item.definitions) {
                    references_in_inlines(inlines, visit);
                }
            }
        }
        Block::FootnoteDefinition { .. }
        | Block::CodeBlock { .. }
        | Block::ThematicBreak
        | Block::HtmlBlock(_)
        | Block::MathBlock(_)
        | Block::PageBreak => {}
    }
}

fn references_in_inlines(inlines: &[Inline], visit: &mut impl FnMut(&str)) {
    for inline in inlines {
        match inline {
            Inline::FootnoteRef { id } => visit(id),
            Inline::Emphasis(content)
            | Inline::Strong(content)
            | Inline::Strikethrough(content)
            | Inline::Link { content, .. } => references_in_inlines(content, visit),
            _ => {}
        }
    }
}

fn rewrite_blocks(blocks: &[Block], notes: &Notes<'_>, numbering: &Numbering) -> Vec<Block> {
    let mut out = Vec::with_capacity(blocks.len());
    for block in blocks {
        let rewritten = match block {
            Block::FootnoteDefinition { .. } => continue,
            Block::Paragraph(inlines) => {
                Block::Paragraph(rewrite_inlines(inlines, notes, numbering))
            }
            Block::Heading { level, inlines } => Block::Heading {
                level: *level,
                inlines: rewrite_inlines(inlines, notes, numbering),
            },
            Block::BlockQuote(inner) => Block::BlockQuote(rewrite_blocks(inner, notes, numbering)),
            Block::List(list) => {
                let mut list = super::provenance::clone_list(list);
                for item in &mut list.items {
                    item.blocks = rewrite_blocks(&item.blocks, notes, numbering);
                }
                Block::List(list)
            }
            Block::Table(table) => {
                let mut table = super::provenance::clone_table(table);
                for cell in &mut table.head {
                    *cell = rewrite_inlines(cell, notes, numbering);
                }
                for row in &mut table.rows {
                    for cell in row {
                        *cell = rewrite_inlines(cell, notes, numbering);
                    }
                }
                Block::Table(table)
            }
            Block::DefinitionList(items) => {
                let mut items = super::provenance::clone_definitions(items);
                for item in &mut items {
                    for inlines in item.terms.iter_mut().chain(&mut item.definitions) {
                        *inlines = rewrite_inlines(inlines, notes, numbering);
                    }
                }
                Block::DefinitionList(items)
            }
            other => super::provenance::clone_block(other),
        };
        out.push(rewritten);
    }
    out
}

/// A note number in Unicode superscript digits (every bundled body face and
/// the SVG/PDF fallback carry them).
fn superscript(number: usize) -> String {
    const DIGITS: [char; 10] = [
        '\u{2070}', '\u{B9}', '\u{B2}', '\u{B3}', '\u{2074}', '\u{2075}', '\u{2076}', '\u{2077}',
        '\u{2078}', '\u{2079}',
    ];
    super::provenance::synthetic(
        number
            .to_string()
            .bytes()
            .map(|digit| DIGITS[usize::from(digit - b'0')])
            .collect(),
    )
}

fn rewrite_inlines(inlines: &[Inline], notes: &Notes<'_>, numbering: &Numbering) -> Vec<Inline> {
    rewrite_inlines_in(inlines, notes, numbering, numbering.paged)
}

/// `link_refs` is false inside an existing link: links do not nest.
fn rewrite_inlines_in(
    inlines: &[Inline],
    notes: &Notes<'_>,
    numbering: &Numbering,
    link_refs: bool,
) -> Vec<Inline> {
    let rewrite_inlines = |content: &[Inline], notes: &Notes<'_>, numbering: &Numbering| {
        rewrite_inlines_in(content, notes, numbering, link_refs)
    };
    inlines
        .iter()
        .map(|inline| match inline {
            Inline::FootnoteRef { id } => match notes.indices.get(id.as_str()) {
                Some(&index) if link_refs => {
                    let number = numbering.numbers[index];
                    Inline::Link {
                        dest: format!("#{PAGE_NOTE_PREFIX}{number}"),
                        title: None,
                        content: vec![Inline::Text(superscript(number))],
                    }
                }
                Some(&index) => Inline::Text(superscript(numbering.numbers[index])),
                None => Inline::Text(super::provenance::visible_footnote(id)),
            },
            Inline::Emphasis(content) => {
                Inline::Emphasis(rewrite_inlines(content, notes, numbering))
            }
            Inline::Strong(content) => Inline::Strong(rewrite_inlines(content, notes, numbering)),
            Inline::Strikethrough(content) => {
                Inline::Strikethrough(rewrite_inlines(content, notes, numbering))
            }
            Inline::Link {
                dest,
                title,
                content,
            } => Inline::Link {
                dest: dest.clone(),
                title: title.clone(),
                content: rewrite_inlines_in(content, notes, numbering, false),
            },
            other => super::provenance::clone_inline(other),
        })
        .collect()
}
