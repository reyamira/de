//! Native FMD rendering for the full Markdown viewer.

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

#[derive(Clone, Debug)]
pub struct RenderRequest {
    pub path: PathBuf,
    pub source: Arc<str>,
    pub width: u32,
    pub height: u32,
    pub scroll_y: u32,
    pub style: Arc<crate::render_theme::RenderedTheme>,
    pub annotations: crate::rendered_text::TextAnnotations,
}

#[derive(Debug)]
pub struct RenderedPage {
    pub image: crate::media::ImagePreview,
    pub scroll_y: u32,
    pub max_scroll: u32,
    pub text: Arc<crate::rendered_text::RenderedText>,
}

#[derive(Default, Debug)]
struct State {
    generation: u64,
    queued: Option<(u64, RenderRequest)>,
    ready: Option<(u64, Result<RenderedPage, String>)>,
    working: bool,
    shutdown: bool,
}

#[derive(Debug)]
pub struct MarkdownRenderer {
    shared: Arc<(Mutex<State>, Condvar)>,
}

impl MarkdownRenderer {
    pub fn new() -> Self {
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let worker = Arc::clone(&shared);
        thread::spawn(move || worker_loop(worker));
        Self { shared }
    }

    pub fn request(&self, request: RenderRequest) -> u64 {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        state.queued = Some((generation, request));
        state.ready = None;
        wake.notify_one();
        generation
    }

    pub fn cancel(&self) {
        let mut state = self.shared.0.lock().unwrap_or_else(|e| e.into_inner());
        state.generation = state.generation.wrapping_add(1);
        state.queued = None;
        state.ready = None;
    }

    pub fn poll(&self) -> Option<(u64, Result<RenderedPage, String>)> {
        self.shared
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .ready
            .take()
    }

    pub fn is_pending(&self) -> bool {
        let state = self.shared.0.lock().unwrap_or_else(|e| e.into_inner());
        state.queued.is_some() || state.ready.is_some() || state.working
    }
}

impl Drop for MarkdownRenderer {
    fn drop(&mut self) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        state.shutdown = true;
        state.queued = None;
        wake.notify_one();
    }
}

fn worker_loop(shared: Arc<(Mutex<State>, Condvar)>) {
    let mut cache = None;
    loop {
        let (generation, request) = {
            let (lock, wake) = &*shared;
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            while state.queued.is_none() && !state.shutdown {
                state = wake.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            if state.shutdown {
                return;
            }
            state.working = true;
            state.queued.take().expect("queued request")
        };
        let mut cancelled = || {
            let state = shared.0.lock().unwrap_or_else(|e| e.into_inner());
            state.shutdown || state.generation != generation
        };
        let result = crate::media::ensure_terminal_support()
            .and_then(|()| render(&request, &mut cache, &mut cancelled));
        let mut state = shared.0.lock().unwrap_or_else(|e| e.into_inner());
        state.working = false;
        if state.shutdown {
            return;
        }
        if state.generation == generation {
            state.ready = Some((generation, result));
        }
    }
}

struct CacheKey {
    path: PathBuf,
    source: Arc<str>,
    width: u32,
    style: Arc<crate::render_theme::RenderedTheme>,
}

impl PartialEq for CacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.source == other.source
            && self.width == other.width
            && Arc::ptr_eq(&self.style, &other.style)
    }
}
impl Eq for CacheKey {}

struct CachedPage {
    key: CacheKey,
    tree: resvg::usvg::Tree,
    scale: f32,
    height: u32,
    text: Arc<crate::rendered_text::RenderedText>,
}

fn render(
    request: &RenderRequest,
    cache: &mut Option<CachedPage>,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<RenderedPage, String> {
    if request.width == 0
        || request.height == 0
        || request.width > 16_384
        || request.height > 16_384
        || u64::from(request.width) * u64::from(request.height) > 16 * 1024 * 1024
    {
        return Err("Markdown viewport is too large to render.".into());
    }
    let key = CacheKey {
        path: request.path.clone(),
        source: Arc::clone(&request.source),
        width: request.width,
        style: Arc::clone(&request.style),
    };
    if cache.as_ref().is_none_or(|page| page.key != key) {
        *cache = None;
        let (mut doc, provenance) = crate::fmd_parse::parse_sourced(&request.source);
        if cancelled() {
            return Err("Markdown rendering cancelled.".into());
        }
        let (assets, provenance) = crate::fmd_parse::with_assets(provenance, || {
            crate::fmd_assets::prepare(&mut doc, &request.path, cancelled)
        });
        let assets = assets?;
        let (doc, provenance) = crate::fmd_parse::prepare_endnotes(doc, provenance);
        let style = &request.style;
        let mut options = crate::fmd_svg::SvgOptions {
            max_width_pt: (request.width as f32 * 0.75).max(1.0),
            heading_scale: f32::from(style.heading_scale_percent) / 100.0,
            code_size_px: Some(style.code_size_px),
            heading_font: style.heading_bytes.clone(),
            heading_color: Some(style.heading.clone()),
            code_text_color: Some(style.code_text.clone()),
            ..Default::default()
        };
        let padding = f32::from(style.page_padding_px) * 0.75;
        options.theme.page.margins = franken_markdown::PageMargins {
            top_pt: padding,
            right_pt: padding,
            bottom_pt: padding,
            left_pt: padding,
        };
        options.theme.spacing.base_px = style.body_size_px;
        options.theme.spacing.line_height = f32::from(style.line_height_percent) / 100.0;
        options.theme.appearance = if style.dark {
            franken_markdown::SystemAppearance::Dark
        } else {
            franken_markdown::SystemAppearance::Light
        };
        let colors = if style.dark {
            &mut options.theme.dark_colors
        } else {
            &mut options.theme.colors
        };
        colors.fg = style.text.clone();
        colors.bg = style.background.clone();
        colors.accent = style.link.clone();
        colors.code_bg = style.code_background.clone();
        colors.quote_fg = style.quote.clone();
        colors.quote_bar = style.quote_bar.clone();
        colors.border = style.table_border.clone();
        colors.stripe = style.table_stripe.clone();
        let (svg, _, _, mut text, (user_width, user_height)) =
            crate::fmd_svg::render_svg_with_text(&doc, &options, &style.fonts, &assets, provenance)
                .map_err(|error| format!("Cannot render Markdown: {error}"))?;
        if cancelled() {
            return Err("Markdown rendering cancelled.".into());
        }
        let tree = resvg::usvg::Tree::from_data(
            &svg,
            &crate::fmd_diagram::options(Arc::clone(&request.style)),
        )
        .map_err(|error| format!("Cannot parse rendered Markdown: {error}"))?;
        let scale = (request.width as f32 / tree.size().width()).min(1.0);
        let height = (tree.size().height() * scale).ceil();
        if !height.is_finite() || height > u32::MAX as f32 {
            return Err("Markdown page is too tall to render.".into());
        }
        // SVG pt dimensions resolve to CSS pixels before the viewport scale.
        // Use the emitted viewBox dimensions, including its 0.01-unit grid.
        text.scale(
            tree.size().width() * scale / user_width,
            tree.size().height() * scale / user_height,
        );
        *cache = Some(CachedPage {
            key,
            tree,
            scale,
            height: height as u32,
            text: Arc::new(text),
        });
    }
    if cancelled() {
        return Err("Markdown rendering cancelled.".into());
    }
    let page = cache.as_ref().expect("rendered page");
    let max_scroll = page.height.saturating_sub(request.height);
    let scroll_y = request.scroll_y.min(max_scroll);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(request.width, request.height)
        .ok_or("Cannot allocate Markdown viewport.")?;
    let rgb = &request.style.background;
    let component = |range| u8::from_str_radix(&rgb[range], 16).expect("validated color");
    pixmap.fill(resvg::tiny_skia::Color::from_rgba8(
        component(1..3),
        component(3..5),
        component(5..7),
        255,
    ));
    resvg::render(
        &page.tree,
        resvg::tiny_skia::Transform::from_row(
            page.scale,
            0.0,
            0.0,
            page.scale,
            0.0,
            -(scroll_y as f32),
        ),
        &mut pixmap.as_mut(),
    );
    paint_annotations(&mut pixmap, &page.text, &request.annotations, scroll_y);
    if cancelled() {
        return Err("Markdown rendering cancelled.".into());
    }
    let png = pixmap
        .encode_png()
        .map_err(|error| format!("Cannot encode Markdown viewport: {error}"))?;
    Ok(RenderedPage {
        image: crate::media::image_from_png(png)?,
        scroll_y,
        max_scroll,
        text: Arc::clone(&page.text),
    })
}

#[cfg(test)]
pub(crate) fn render_test_page(request: &RenderRequest) -> Result<RenderedPage, String> {
    // Exercise native rendering without the worker's terminal-display policy.
    render(request, &mut None, &mut || false)
}

fn paint_annotations(
    pixmap: &mut resvg::tiny_skia::Pixmap,
    text: &crate::rendered_text::RenderedText,
    annotations: &crate::rendered_text::TextAnnotations,
    scroll_y: u32,
) {
    let width = pixmap.width() as f32;
    let height = pixmap.height() as f32;
    let mut paint = resvg::tiny_skia::Paint::default();
    let mut draw = |range: &std::ops::Range<usize>, color: (u8, u8, u8, u8)| {
        paint.set_color_rgba8(color.0, color.1, color.2, color.3);
        for rect in text.rectangles(range) {
            let left = rect.x.max(0.0);
            let top = (rect.y - scroll_y as f32).max(0.0);
            let right = (rect.x + rect.width).min(width);
            let bottom = (rect.y + rect.height - scroll_y as f32).min(height);
            if let Some(rect) =
                resvg::tiny_skia::Rect::from_xywh(left, top, right - left, bottom - top)
            {
                pixmap.fill_rect(rect, &paint, resvg::tiny_skia::Transform::identity(), None);
            }
        }
    };
    for range in annotations.matches.iter() {
        draw(range, (255, 200, 0, 70));
    }
    if let Some(range) = &annotations.active {
        draw(range, (255, 140, 0, 100));
    }
    if let Some(range) = &annotations.selection {
        draw(range, (70, 130, 255, 100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(dark: bool) -> Arc<crate::render_theme::RenderedTheme> {
        crate::render_theme::RenderedTheme::defaults(dark)
            .resolve(std::path::Path::new("."))
            .unwrap()
    }

    #[test]
    fn compact_margins_leave_more_width_for_text() {
        let source = "A paragraph with enough words to wrap across several lines. ".repeat(20);
        let request = RenderRequest {
            path: PathBuf::from("/tmp/test.md"),
            source: Arc::from(source),
            width: 320,
            height: 180,
            scroll_y: 0,
            style: style(false),
            annotations: Default::default(),
        };
        let mut cache = None;
        let page = render(&request, &mut cache, &mut || false).unwrap();
        assert_eq!((page.image.width, page.image.height), (320, 180));
        let compact = cache.as_ref().unwrap();
        assert!((compact.tree.size().width() - 320.0).abs() < 0.1);

        let doc = franken_markdown::parse_markdown(&request.source);
        let options = franken_markdown::svg::SvgOptions {
            max_width_pt: 240.0,
            ..Default::default()
        };
        let (svg, _, _) = franken_markdown::svg::render_svg_with_resources(
            &doc,
            &options,
            &franken_markdown::FontAssets::default(),
            &[],
        )
        .unwrap();
        let default_tree =
            resvg::usvg::Tree::from_data(&svg, &crate::fmd_diagram::options(style(false))).unwrap();
        assert!(compact.tree.size().height() < default_tree.size().height());
    }

    #[test]
    fn narrow_figure_viewports_reflow_without_page_zoom_and_keep_scroll_cache() {
        let mut request = RenderRequest {
            path: PathBuf::from("/tmp/figures.md"),
            source: Arc::from(
                "Before\n\n```mermaid\nflowchart LR\n A[Alpha] --> B[Beta]\n```\n\n\
                 After\n\n```mermaid\nsequenceDiagram\n A->>B: Hello\n```\n\n\
                 ```mermaid\npie\n \"Used\" : 3\n \"Free\" : 1\n```\n\nEnd\n",
            ),
            width: 128,
            height: 80,
            scroll_y: 0,
            style: style(false),
            annotations: Default::default(),
        };
        let mut cache = None;
        for width in [128, 640, 128] {
            request.width = width;
            request.scroll_y = 0;
            let first = render(&request, &mut cache, &mut || false).unwrap();
            let cached = cache.as_ref().unwrap();
            assert!((cached.tree.size().width() - width as f32).abs() < 0.1);
            assert!((cached.scale - 1.0).abs() < 0.00001);
            assert!(first.max_scroll > 0);
            let tree = &cached.tree as *const _;
            request.scroll_y = u32::MAX;
            let last = render(&request, &mut cache, &mut || false).unwrap();
            assert_eq!(last.scroll_y, last.max_scroll);
            assert_eq!(tree, &cache.as_ref().unwrap().tree as *const _);
            assert_ne!(first.image.encoded, last.image.encoded);
        }
    }

    #[test]
    fn renders_viewports_reuses_layout_and_reflows() {
        let source = "# Title\n\nA long paragraph with 日本語 and emoji 😀.\n\n".repeat(80);
        let mut request = RenderRequest {
            path: PathBuf::from("/tmp/test.md"),
            source: Arc::from(source),
            width: 500,
            height: 180,
            scroll_y: 0,
            style: style(false),
            annotations: Default::default(),
        };
        let mut cache = None;
        let first = render(&request, &mut cache, &mut || false).unwrap();
        assert_eq!((first.image.width, first.image.height), (500, 180));
        assert!(first.max_scroll > 0);
        let first_tree = &cache.as_ref().unwrap().tree as *const _;
        request.scroll_y = u32::MAX;
        let last = render(&request, &mut cache, &mut || false).unwrap();
        assert_eq!(last.scroll_y, last.max_scroll);
        assert_eq!(first_tree, &cache.as_ref().unwrap().tree as *const _);
        assert_ne!(first.image.encoded, last.image.encoded);
        request.width = 320;
        let resized = render(&request, &mut cache, &mut || false).unwrap();
        assert_eq!(resized.image.width, 320);
        assert_eq!(cache.as_ref().unwrap().key.width, 320);
        request.style = style(true);
        let dark = render(&request, &mut cache, &mut || false).unwrap();
        assert_ne!(resized.image.encoded, dark.image.encoded);
        assert!(cache.as_ref().unwrap().key.style.dark);
        assert!(render(&request, &mut cache, &mut || true).is_err());
    }

    #[test]
    fn changed_typography_and_colors_replace_cached_layout() {
        let mut request = RenderRequest {
            path: PathBuf::from("/tmp/test.md"),
            source: Arc::from("# A heading\n\nA paragraph with several words. ".repeat(40)),
            width: 400,
            height: 160,
            scroll_y: 0,
            style: style(false),
            annotations: Default::default(),
        };
        let mut cache = None;
        let first = render(&request, &mut cache, &mut || false).unwrap();
        let old_height = cache.as_ref().unwrap().height;
        let mut changed = crate::render_theme::RenderedTheme::defaults(false);
        changed.body_size_px = 24;
        changed.heading_scale_percent = 150;
        changed.background = "#123456".into();
        request.style = changed.resolve(std::path::Path::new(".")).unwrap();
        let second = render(&request, &mut cache, &mut || false).unwrap();
        assert_ne!(first.image.encoded, second.image.encoded);
        assert_ne!(old_height, cache.as_ref().unwrap().height);
        assert!(Arc::ptr_eq(
            &cache.as_ref().unwrap().key.style,
            &request.style
        ));
    }

    #[test]
    fn dark_mermaid_has_visible_nodes_and_labels() {
        use base64::Engine;
        let request = RenderRequest {
            path: PathBuf::from("/tmp/test.md"),
            source: Arc::from("```mermaid\nflowchart LR\n A[Alpha] --> B[Beta]\n```\n"),
            width: 600,
            height: 300,
            scroll_y: 0,
            style: style(true),
            annotations: Default::default(),
        };
        let page = render(&request, &mut None, &mut || false).unwrap();
        let png = base64::engine::general_purpose::STANDARD
            .decode(&*page.image.encoded)
            .unwrap();
        let image = image::load_from_memory(&png).unwrap().to_rgb8();
        let nodes = image.pixels().filter(|p| p.0 == [22, 27, 34]).count();
        let labels = image
            .pixels()
            .filter(|p| p.0[0] > 180 && p.0[1] > 180 && p.0[2] > 180)
            .count();
        assert!(nodes > 100, "diagram node fill was not visible: {nodes}");
        assert!(labels > 10, "diagram text was not visible: {labels}");
    }
}

#[cfg(test)]
mod text_interaction_tests {
    use super::*;
    use base64::Engine;
    use std::ops::Range;

    fn request(source: &str, width: u32) -> RenderRequest {
        RenderRequest {
            path: PathBuf::from("/tmp/native-text.md"),
            source: Arc::from(source),
            width,
            height: 100,
            scroll_y: 0,
            style: crate::render_theme::RenderedTheme::defaults(false)
                .resolve(std::path::Path::new("."))
                .unwrap(),
            annotations: Default::default(),
        }
    }

    fn ranges(text: &str, query: &str) -> Vec<Range<usize>> {
        text.match_indices(query)
            .map(|(start, value)| start..start + value.len())
            .collect()
    }

    #[test]
    fn definition_blocks_keep_copy_and_find_boundaries() {
        let source = "Intro\n\nTerm\n: First definition\n: Second definition\n\nAfter\n";
        let page = render(&request(source, 400), &mut None, &mut || false).unwrap();
        assert_eq!(
            page.text.text(),
            "Intro\nTerm\nFirst definition\nSecond definition\nAfter"
        );
        for false_match in [
            "IntroTerm",
            "TermFirst",
            "definitionSecond",
            "definitionAfter",
        ] {
            assert!(ranges(page.text.text(), false_match).is_empty());
        }
        let definitions = ranges(page.text.text(), "definition");
        assert_eq!(definitions.len(), 2);
        for range in definitions {
            assert!(!page.text.rectangles(&range).is_empty());
        }
    }

    #[test]
    fn empty_table_cells_preserve_all_copy_columns_and_rows() {
        let source = "| A | | C | |\n| --- | --- | --- | --- |\n\
            | left | | right | |\n| | | | |\n| tail |\n";
        let page = render(&request(source, 500), &mut None, &mut || false).unwrap();
        assert_eq!(
            page.text.text(),
            "A\t\tC\t\nleft\t\tright\t\n\t\t\t\ntail\t\t\t"
        );
        assert!(ranges(page.text.text(), "leftright").is_empty());
        assert_eq!(ranges(page.text.text(), "left\t\tright").len(), 1);
        let right = ranges(page.text.text(), "right")[0].clone();
        assert!(!page.text.rectangles(&right).is_empty());
    }

    #[test]
    fn image_fallback_in_a_native_table_is_copied_once_in_cell_order() {
        let source = "| A | B |\n| --- | --- |\n| ![alt](../unavailable.png) | end |\n";
        let page = render(&request(source, 500), &mut None, &mut || false).unwrap();
        assert_eq!(
            page.text.text(),
            "A\tB\n[Image unavailable or image budget exceeded: alt]\tend"
        );
        let matches = ranges(page.text.text(), "alt");
        assert_eq!(matches.len(), 1);
        let source_start = source.find("alt").unwrap();
        assert_eq!(
            page.text
                .source_match(source_start..source_start + 3, &matches),
            Some(matches[0].clone())
        );
        assert!(!page.text.rectangles(&matches[0]).is_empty());
    }

    #[test]
    fn native_source_identity_survives_nested_markup_entities_tables_and_reflow() {
        let source = "# same\n\n[same](https://hidden.invalid/same) and **same** &amp; e\u{301}.\n\
            A soft\nbreak and \\*literal\\*.\n\n\
            ```rust\nsame\tvalue\n```\n\n\
            | Label | Value |\n| --- | --- |\n| same | `b\\|c` |\n";
        let mut request = request(source, 500);
        let mut cache = None;
        let wide = render(&request, &mut cache, &mut || false).unwrap();
        assert!(
            wide.text
                .text()
                .starts_with("same\nsame and same & e\u{301}.")
        );
        assert!(wide.text.text().contains("A soft break and *literal*."));
        assert!(wide.text.text().contains("Label\tValue\nsame\tb|c"));
        assert!(!wide.text.text().contains("hidden.invalid"));
        let visible = ranges(wide.text.text(), "same");
        let raw = ranges(source, "same");
        assert_eq!(visible.len(), 5);
        for (raw, visible) in [
            (&raw[0], &visible[0]),
            (&raw[1], &visible[1]),
            (&raw[3], &visible[2]),
            (&raw[4], &visible[3]),
            (&raw[5], &visible[4]),
        ] {
            assert_eq!(
                wide.text
                    .source_match(raw.clone(), &ranges(wide.text.text(), "same")),
                Some(visible.clone())
            );
            let rectangles = wide.text.rectangles(visible);
            assert!(!rectangles.is_empty());
            assert!(rectangles.iter().all(|rect| rect.width > 0.0
                && rect.height > 0.0
                && rect.x >= 0.0
                && rect.x + rect.width <= request.width as f32 + 0.1));
        }
        assert_eq!(wide.text.source_match(raw[2].clone(), &visible), None);
        let amp = source.find("&amp;").unwrap();
        assert_eq!(
            wide.text
                .source_match(amp..amp + 1, &ranges(wide.text.text(), "&")),
            ranges(wide.text.text(), "&").first().cloned()
        );
        let pipe = source.find("\\|").unwrap() + 1;
        assert_eq!(
            wide.text
                .source_match(pipe..pipe + 1, &ranges(wide.text.text(), "|")),
            ranges(wide.text.text(), "|").first().cloned()
        );
        let composed = ranges(wide.text.text(), "e\u{301}")[0].clone();
        let rect = wide.text.rectangles(&composed)[0];
        let left = wide
            .text
            .hit_test(rect.x + rect.width * 0.25, rect.y + rect.height * 0.5)
            .unwrap();
        let right = wide
            .text
            .hit_test(rect.x + rect.width * 0.75, rect.y + rect.height * 0.5)
            .unwrap();
        assert!(wide.text.text().is_char_boundary(left));
        assert!(wide.text.text().is_char_boundary(right));
        assert!(left <= right);
        request.width = 180;
        let narrow = render(&request, &mut cache, &mut || false).unwrap();
        assert_eq!(wide.text.text(), narrow.text.text());
        assert_ne!(
            wide.text.rectangles(&visible[4]),
            narrow.text.rectangles(&visible[4])
        );
        assert_eq!(
            narrow
                .text
                .source_match(raw[5].clone(), &ranges(narrow.text.text(), "same")),
            Some(visible[4].clone())
        );
    }

    #[test]
    fn native_graphics_have_no_invented_glyphs_but_text_fallbacks_are_selectable() {
        let source = "Before $x^2$ after.\n\n```mermaid\nflowchart LR\n A[DiagramNode] --> B[OtherNode]\n```\n\n\
            ![missing label](missing-file.png)\n\nUnsupported $\\definitelyunsupported{fallback}$.\n";
        let page = render(&request(source, 500), &mut None, &mut || false).unwrap();
        assert!(!page.text.text().contains("x^2"));
        assert!(!page.text.text().contains("DiagramNode"));
        assert!(
            page.text
                .text()
                .contains("Image unavailable or image budget exceeded: missing label")
        );
        for query in ["missing label", "fallback"] {
            let visible = ranges(page.text.text(), query);
            assert_eq!(visible.len(), 1);
            let start = source.find(query).unwrap();
            assert_eq!(
                page.text.source_match(start..start + query.len(), &visible),
                visible.first().cloned()
            );
            assert!(!page.text.rectangles(&visible[0]).is_empty());
        }
    }

    #[test]
    fn container_copies_keep_crlf_bom_and_code_tab_provenance() {
        let source = "\u{feff}> **same** &amp;\r\n> next\r\n\r\n- same\r\n\r\n    same A\tB\r\n";
        let page = render(&request(source, 360), &mut None, &mut || false).unwrap();
        let visible = ranges(page.text.text(), "same");
        let raw = ranges(source, "same");
        assert_eq!(visible.len(), 3);
        for (source, expected) in raw.iter().zip(&visible) {
            assert_eq!(
                page.text.source_match(source.clone(), &visible),
                Some(expected.clone())
            );
            assert!(!page.text.rectangles(expected).is_empty());
        }
        assert!(page.text.text().contains("same & next"));
        assert!(page.text.text().contains("same A\tB"));
        let newline = source.find("\r\n").unwrap();
        let space = page
            .text
            .source_match(newline..newline + 2, &ranges(page.text.text(), " "))
            .unwrap();
        assert_eq!(&page.text.text()[space], " ");
        let tab = ranges(page.text.text(), "\t")[0].clone();
        assert!(
            page.text
                .rectangles(&tab)
                .iter()
                .map(|rect| rect.width)
                .sum::<f32>()
                > 0.0
        );
    }

    #[test]
    fn native_endnotes_keep_their_own_source_occurrence() {
        let source = "same[^b] and same[^a].\n\n[^a]: same\n\n[^b]: same\n";
        let page = render(&request(source, 400), &mut None, &mut || false).unwrap();
        let visible = ranges(page.text.text(), "same");
        let raw = ranges(source, "same");
        assert_eq!(visible.len(), 4);
        assert_eq!(
            page.text.source_match(raw[2].clone(), &visible),
            Some(visible[3].clone())
        );
        assert_eq!(
            page.text.source_match(raw[3].clone(), &visible),
            Some(visible[2].clone())
        );
    }

    fn rgba(page: &RenderedPage) -> image::RgbaImage {
        let png = base64::engine::general_purpose::STANDARD
            .decode(&*page.image.encoded)
            .unwrap();
        image::load_from_memory(&png).unwrap().to_rgba8()
    }

    #[test]
    fn viewport_annotations_use_full_page_geometry_and_keep_cached_layout() {
        let source = "A paragraph with words and enough wrapping space.\n\n".repeat(15);
        let mut request = request(&source, 260);
        let mut cache = None;
        let first = render(&request, &mut cache, &mut || false).unwrap();
        let range = ranges(first.text.text(), "words").pop().unwrap();
        let target = first.text.rectangles(&range)[0];
        request.scroll_y = target.y.max(0.0) as u32;
        let plain = render(&request, &mut cache, &mut || false).unwrap();
        request.annotations.selection = Some(range.clone());
        let marked = render(&request, &mut cache, &mut || false).unwrap();
        assert_eq!(plain.text.text(), marked.text.text());
        assert_eq!(
            plain.text.rectangles(&range),
            marked.text.rectangles(&range)
        );
        assert_eq!(plain.scroll_y, marked.scroll_y);
        assert!(marked.scroll_y > 0);
        let before = rgba(&plain);
        let after = rgba(&marked);
        let rectangles = marked.text.rectangles(&range);
        let mut changed = 0;
        for (x, y, pixel) in before.enumerate_pixels() {
            if pixel == after.get_pixel(x, y) {
                continue;
            }
            changed += 1;
            assert!(rectangles.iter().any(|rect| {
                let page_y = y as f32 + marked.scroll_y as f32;
                x as f32 + 1.0 >= rect.x
                    && (x as f32) <= rect.x + rect.width
                    && page_y + 1.0 >= rect.y
                    && page_y <= rect.y + rect.height
            }));
        }
        assert!(changed > 10);
    }
}
