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
    pub dark: bool,
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

#[derive(PartialEq, Eq)]
struct CacheKey {
    path: PathBuf,
    source: Arc<str>,
    width: u32,
    dark: bool,
}

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
        dark: request.dark,
    };
    if cache.as_ref().is_none_or(|page| page.key != key) {
        *cache = None;
        let mut doc = franken_markdown::parse_markdown(&request.source);
        if cancelled() {
            return Err("Markdown rendering cancelled.".into());
        }
        let assets = crate::fmd_assets::prepare(&mut doc, &request.path, cancelled)?;
        let mut options = franken_markdown::svg::SvgOptions {
            max_width_pt: (request.width as f32 * 0.75).max(144.0),
            ..Default::default()
        };
        options.theme.appearance = if request.dark {
            franken_markdown::SystemAppearance::Dark
        } else {
            franken_markdown::SystemAppearance::Light
        };
        let (svg, _, _) = franken_markdown::svg::render_svg_with_resources(
            &doc,
            &options,
            &franken_markdown::FontAssets::default(),
            &assets,
        )
        .map_err(|error| format!("Cannot render Markdown: {error}"))?;
        if cancelled() {
            return Err("Markdown rendering cancelled.".into());
        }
        let tree = resvg::usvg::Tree::from_data(&svg, &crate::fmd_diagram::options(request.dark))
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
    pixmap.fill(if request.dark {
        resvg::tiny_skia::Color::from_rgba8(13, 17, 23, 255)
    } else {
        resvg::tiny_skia::Color::WHITE
    });
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

    #[test]
    fn renders_viewports_reuses_layout_and_reflows() {
        let source = "# Title\n\nA long paragraph with 日本語 and emoji 😀.\n\n".repeat(80);
        let mut request = RenderRequest {
            path: PathBuf::from("/tmp/test.md"),
            source: Arc::from(source),
            width: 500,
            height: 180,
            scroll_y: 0,
            dark: false,
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
        request.dark = true;
        let dark = render(&request, &mut cache, &mut || false).unwrap();
        assert_ne!(resized.image.encoded, dark.image.encoded);
        assert!(cache.as_ref().unwrap().key.dark);
        assert!(render(&request, &mut cache, &mut || true).is_err());
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
            dark: true,
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
