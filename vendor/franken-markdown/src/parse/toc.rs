//! Shared marker recognition and document-scoped TOC placement decisions.

use franken_markdown::ast::{Block, Inline};

/// A marker is a whole paragraph, never an inline substring or code span.
/// The optional depth is the greatest source heading level to include.
pub(crate) fn marker_depth(inlines: &[Inline]) -> Option<Option<u8>> {
    let [Inline::Text(text)] = inlines else {
        return None;
    };
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .or_else(|| trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')))?
        .trim_start_matches('_')
        .trim_end_matches('_')
        .to_ascii_lowercase();
    let inner = inner.strip_prefix("toc")?;
    if inner.is_empty() {
        return Some(None);
    }
    let depth = inner.strip_prefix(':')?.trim().parse::<u8>().ok()?;
    (depth >= 1).then_some(Some(depth))
}
