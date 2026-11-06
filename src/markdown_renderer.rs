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
}

#[derive(Debug)]
pub struct RenderedPage {
    pub image: crate::media::ImagePreview,
    pub scroll_y: u32,
    pub max_scroll: u32,
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
        let mut doc = franken_markdown::parse_markdown(&request.source);
        if cancelled() {
            return Err("Markdown rendering cancelled.".into());
        }
        let assets = crate::fmd_assets::prepare(&mut doc, &request.path, cancelled)?;
        let style = &request.style;
        let mut options = crate::fmd_svg::SvgOptions {
            max_width_pt: (request.width as f32 * 0.75).max(144.0),
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
        let (svg, _, _) =
            crate::fmd_svg::render_svg_with_resources(&doc, &options, &style.fonts, &assets)
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
        *cache = Some(CachedPage {
            key,
            tree,
            scale,
            height: height as u32,
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
    })
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
    fn renders_viewports_reuses_layout_and_reflows() {
        let source = "# Title\n\nA long paragraph with 日本語 and emoji 😀.\n\n".repeat(80);
        let mut request = RenderRequest {
            path: PathBuf::from("/tmp/test.md"),
            source: Arc::from(source),
            width: 500,
            height: 180,
            scroll_y: 0,
            style: style(false),
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
