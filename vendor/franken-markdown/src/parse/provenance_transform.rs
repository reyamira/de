//! Preserve identities through host assets and upstream endnote preparation.
use super::*;

pub(crate) fn with_assets<T>(origins: Provenance, prepare: impl FnOnce() -> T) -> (T, Provenance) {
    OUTPUT.with(|output| *output.borrow_mut() = origins);
    let result = prepare();
    let origins = OUTPUT.with(|output| std::mem::take(&mut *output.borrow_mut()));
    (result, origins)
}

pub(crate) fn image_fallback(alt: &str) -> String {
    let mut result = "[Image unavailable or image budget exceeded: ".to_owned();
    register(&result, Vec::new());
    append(&mut result, alt);
    let origins = OUTPUT.with(|output| output.borrow().get(&result));
    result.push(']');
    register(&result, origins);
    result
}

pub(crate) fn prepare_endnotes(document: Document, origins: Provenance) -> (Document, Provenance) {
    with_assets(
        origins,
        || match super::super::footnotes::endnotes_for_pdf(&document, false) {
            std::borrow::Cow::Owned(prepared) => prepared,
            std::borrow::Cow::Borrowed(_) => document,
        },
    )
}

pub(in super::super) fn soft_break(
    chars: &[char],
    index: usize,
    profiler: &ParseProfiler,
) -> Inline {
    if profiler.destinations.is_none() {
        return Inline::SoftBreak;
    }
    let text = " ".to_owned();
    register(
        &text,
        char_origins(&text, &chars[index..index + 1], profiler),
    );
    Inline::Text(text)
}

fn inherit_text(original: &str, copied: &str) {
    let origins = OUTPUT.with(|output| output.borrow().get(original));
    register(copied, origins);
}

fn inherit_inlines(original: &[Inline], copied: &[Inline]) {
    for (original, copied) in original.iter().zip(copied) {
        inherit_inline(original, copied);
    }
}

fn inherit_inline(original: &Inline, copied: &Inline) {
    match (original, copied) {
        (Inline::Text(a), Inline::Text(b))
        | (Inline::Code(a), Inline::Code(b))
        | (Inline::Html(a), Inline::Html(b))
        | (Inline::Math(a), Inline::Math(b))
        | (Inline::DisplayMath(a), Inline::DisplayMath(b)) => inherit_text(a, b),
        (Inline::FootnoteRef { id: a }, Inline::FootnoteRef { id: b }) => inherit_text(a, b),
        (Inline::Emphasis(a), Inline::Emphasis(b))
        | (Inline::Strong(a), Inline::Strong(b))
        | (Inline::Strikethrough(a), Inline::Strikethrough(b)) => inherit_inlines(a, b),
        (Inline::Link { content: a, .. }, Inline::Link { content: b, .. }) => inherit_inlines(a, b),
        (Inline::Image { alt: a, .. }, Inline::Image { alt: b, .. }) => inherit_text(a, b),
        _ => {}
    }
}

fn inherit_block(original: &Block, copied: &Block) {
    match (original, copied) {
        (Block::Paragraph(a), Block::Paragraph(b))
        | (Block::Heading { inlines: a, .. }, Block::Heading { inlines: b, .. }) => {
            inherit_inlines(a, b)
        }
        (Block::CodeBlock { code: a, .. }, Block::CodeBlock { code: b, .. })
        | (Block::MathBlock(a), Block::MathBlock(b))
        | (Block::HtmlBlock(a), Block::HtmlBlock(b)) => inherit_text(a, b),
        (Block::BlockQuote(a), Block::BlockQuote(b))
        | (
            Block::FootnoteDefinition { blocks: a, .. },
            Block::FootnoteDefinition { blocks: b, .. },
        ) => {
            for (a, b) in a.iter().zip(b) {
                inherit_block(a, b);
            }
        }
        (Block::List(a), Block::List(b)) => inherit_list(a, b),
        (Block::Table(a), Block::Table(b)) => inherit_table(a, b),
        (Block::DefinitionList(a), Block::DefinitionList(b)) => inherit_definitions(a, b),
        _ => {}
    }
}

fn inherit_list(a: &List, b: &List) {
    for (a, b) in a.items.iter().zip(&b.items) {
        for (a, b) in a.blocks.iter().zip(&b.blocks) {
            inherit_block(a, b);
        }
    }
}

fn inherit_table(a: &Table, b: &Table) {
    for (a, b) in a.head.iter().zip(&b.head) {
        inherit_inlines(a, b);
    }
    for (a, b) in a.rows.iter().zip(&b.rows) {
        for (a, b) in a.iter().zip(b) {
            inherit_inlines(a, b);
        }
    }
}

fn inherit_definitions(
    a: &[franken_markdown::DefinitionItem],
    b: &[franken_markdown::DefinitionItem],
) {
    for (a, b) in a.iter().zip(b) {
        for (a, b) in a
            .terms
            .iter()
            .chain(&a.definitions)
            .zip(b.terms.iter().chain(&b.definitions))
        {
            inherit_inlines(a, b);
        }
    }
}

pub(in super::super) fn clone_inline(original: &Inline) -> Inline {
    let copied = original.clone();
    inherit_inline(original, &copied);
    copied
}

pub(in super::super) fn clone_block(original: &Block) -> Block {
    let copied = original.clone();
    inherit_block(original, &copied);
    copied
}

pub(in super::super) fn clone_list(original: &List) -> List {
    let copied = original.clone();
    inherit_list(original, &copied);
    copied
}

pub(in super::super) fn clone_table(original: &Table) -> Table {
    let copied = original.clone();
    inherit_table(original, &copied);
    copied
}

pub(in super::super) fn clone_definitions(
    original: &[franken_markdown::DefinitionItem],
) -> Vec<franken_markdown::DefinitionItem> {
    let copied = original.to_vec();
    inherit_definitions(original, &copied);
    copied
}
