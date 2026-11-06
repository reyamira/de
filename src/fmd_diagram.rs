//! Adapt FMD's generated diagram SVGs to usvg's static CSS and font support.
//!
//! Only FMD-generated SVG images reach this resolver. User images are decoded
//! and normalized to PNG before FMD sees them. No file or network access is
//! allowed by either the outer or nested image resolver.

use resvg::usvg::{self, ImageHrefResolver, ImageKind, Options, Tree};
use std::sync::{Arc, LazyLock};

fn fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
        let mut db = usvg::fontdb::Database::new();
        for bytes in [
            fmd_font::bundled::PLEX_REGULAR,
            fmd_font::bundled::PLEX_BOLD,
            fmd_font::bundled::CM_TYPEWRITER,
            fmd_font::bundled::noto_sans_cjk_sc(),
            fmd_font::bundled::NOTO_EMOJI_CURATED,
        ] {
            db.load_font_data(bytes.to_vec());
        }
        db.set_serif_family("IBM Plex Sans");
        db.set_sans_serif_family("IBM Plex Sans");
        db.set_monospace_family("CMU Typewriter Text");
        Arc::new(db)
    });
    Arc::clone(&FONTS)
}

pub(crate) fn options(style: Arc<crate::render_theme::RenderedTheme>) -> Options<'static> {
    let default_data = ImageHrefResolver::default_data_resolver();
    Options {
        fontdb: fonts(),
        image_href_resolver: ImageHrefResolver {
            resolve_data: Box::new(move |mime, data, opt| {
                if mime != "image/svg+xml" {
                    return default_data(mime, data, opt);
                }
                let source = std::str::from_utf8(&data).ok()?;
                let adapted = adapt_svg(source, &style);
                let nested = Options {
                    fontdb: Arc::clone(&opt.fontdb),
                    image_href_resolver: ImageHrefResolver {
                        resolve_data: Box::new(|_, _, _| None),
                        resolve_string: Box::new(|_, _| None),
                    },
                    ..Options::default()
                };
                Tree::from_str(&adapted, &nested).ok().map(ImageKind::SVG)
            }),
            resolve_string: Box::new(|_, _| None),
        },
        ..Options::default()
    }
}

// FMD emits static styles with CSS variables and a dark media block. usvg does
// not evaluate either, so resolve those rules before parsing the nested SVG.
fn adapt_svg(svg: &str, style: &crate::render_theme::RenderedTheme) -> String {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(start) = rest.find("<style>") {
        let (before, after) = rest.split_at(start + "<style>".len());
        out.push_str(before);
        let Some(end) = after.find("</style>") else {
            out.push_str(after);
            return out;
        };
        out.push_str(&resolve_css(&after[..end], style));
        rest = &after[end..];
    }
    out.push_str(rest);
    out.replace(
        "fill=\"var(--fg-muted, #57606a)\"",
        &format!("fill=\"{}\"", style.quote),
    )
}

fn resolve_css(css: &str, style: &crate::render_theme::RenderedTheme) -> String {
    let mut flattened = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("@media") {
        flattened.push_str(&rest[..start]);
        let block = &rest[start..];
        let Some(open) = block.find('{') else {
            flattened.push_str(block);
            return flattened;
        };
        let mut depth = 1usize;
        let mut close = None;
        for (index, byte) in block.bytes().enumerate().skip(open + 1) {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = close else {
            flattened.push_str(block);
            return flattened;
        };
        rest = &block[end + 1..];
    }
    flattened.push_str(rest);
    let mut out = String::with_capacity(flattened.len());
    let mut rest = flattened.as_str();
    while let Some(start) = rest.find("var(") {
        out.push_str(&rest[..start]);
        let expression = &rest[start + 4..];
        let Some(end) = expression.find(')') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let (name, fallback) = expression[..end].split_once(',').unwrap_or(("", ""));
        let color = match name.trim() {
            "--fg-default" => style.text.as_str(),
            "--fg-muted" => style.quote.as_str(),
            "--bg-canvas" => style.code_background.as_str(),
            "--bg-subtle" => style.code_background.as_str(),
            "--border-default" => style.table_border.as_str(),
            _ => fallback.trim(),
        };
        out.push_str(color);
        rest = &expression[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagram_css_uses_configured_colors_in_both_appearances() {
        for dark in [false, true] {
            let mut style = crate::render_theme::RenderedTheme::defaults(dark);
            style.text = "#123456".into();
            style.code_background = "#abcdef".into();
            let css = ".node { fill: var(--bg-canvas, #fff); color: var(--fg-default, #000); } @media (prefers-color-scheme: dark) { .node { fill: #000; } }";
            let result = resolve_css(css, &style);
            assert!(result.contains("#123456"));
            assert!(result.contains("#abcdef"));
            assert!(!result.contains("fill: #000"));
        }
    }
}
