//! Isolated Chromium rendering for the full Markdown viewer.

use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, Cursor, Read};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD};
use image::ImageDecoder;
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};
use serde_json::{Value, json};
use tungstenite::{
    Message, WebSocket, client::client_with_config, protocol::WebSocketConfig,
    stream::MaybeTlsStream,
};

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
    child: Option<Child>,
    profile: Option<tempfile::TempDir>,
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
        reap_child(&mut state);
        wake.notify_one();
    }
}

fn worker_loop(shared: Arc<(Mutex<State>, Condvar)>) {
    let mut browser: Option<Browser> = None;
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
        let result = crate::media::ensure_terminal_support().and_then(|()| {
            if browser.is_none() {
                browser = Some(Browser::launch(Arc::clone(&shared))?);
            }
            let result = browser
                .as_mut()
                .expect("browser launched")
                .render(&request, generation);
            if result
                .as_ref()
                .is_err_and(|e| e != "Markdown rendering cancelled.")
            {
                browser = None;
            }
            result
        });
        let mut state = shared.0.lock().unwrap_or_else(|e| e.into_inner());
        if !state.finish(generation, result) {
            return;
        }
    }
}

impl State {
    fn finish(&mut self, generation: u64, result: Result<RenderedPage, String>) -> bool {
        self.working = false;
        if self.shutdown {
            return false;
        }
        if self.generation == generation {
            self.ready = Some((generation, result));
        }
        true
    }
}

struct Browser {
    // The shared registry lets renderer Drop stop Chromium even during startup.
    shared: Arc<(Mutex<State>, Condvar)>,
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    session: String,
    next_id: u64,
    cache: Option<CacheKey>,
    document_height: u32,
}

#[derive(PartialEq, Eq)]
struct CacheKey {
    path: PathBuf,
    source: Arc<str>,
    width: u32,
    height: u32,
    dark: bool,
}

impl Drop for Browser {
    fn drop(&mut self) {
        let mut state = self.shared.0.lock().unwrap_or_else(|e| e.into_inner());
        reap_child(&mut state);
    }
}

fn reap_child(state: &mut State) {
    if let Some(mut child) = state.child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(state.profile.take());
}

// Reap Chromium on all failed startup paths as well.
struct ChildGuard {
    shared: Arc<(Mutex<State>, Condvar)>,
    armed: bool,
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.armed {
            let mut state = self.shared.0.lock().unwrap_or_else(|e| e.into_inner());
            reap_child(&mut state);
        }
    }
}

fn chromium_path() -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let executable = |p: &Path| {
        fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in [
                "chromium",
                "chromium-browser",
                "google-chrome",
                "google-chrome-stable",
            ] {
                let candidate = dir.join(name);
                if executable(&candidate) {
                    return Ok(candidate);
                }
            }
        }
    }
    for name in [
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
    ] {
        let candidate = PathBuf::from(name);
        if executable(&candidate) {
            return Ok(candidate);
        }
    }
    Err("Chromium was not found. Install Chromium or Google Chrome, or press m for source or o to open externally.".into())
}

impl Browser {
    fn launch(shared: Arc<(Mutex<State>, Condvar)>) -> Result<Self, String> {
        let executable = chromium_path()?;
        // Serialize spawn and registration with shutdown so Drop cannot miss a child.
        let mut state = shared.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.shutdown {
            return Err("Markdown renderer is shutting down.".into());
        }
        let profile =
            tempfile::tempdir().map_err(|e| format!("Cannot create Chromium profile: {e}"))?;
        let profile_path = profile.path().to_owned();
        let child = Command::new(executable)
            .args([
                "--headless",
                "--disable-gpu",
                "--disable-background-networking",
                "--disable-component-update",
                "--disable-sync",
                "--no-first-run",
                "--no-default-browser-check",
                "--host-resolver-rules=MAP * ~NOTFOUND",
                "--remote-debugging-address=127.0.0.1",
                "--remote-debugging-port=0",
            ])
            .arg(format!("--user-data-dir={}", profile_path.display()))
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Cannot start Chromium: {e}"))?;
        state.child = Some(child);
        state.profile = Some(profile);
        drop(state);
        let mut child = ChildGuard {
            shared: Arc::clone(&shared),
            armed: true,
        };
        let deadline = Instant::now() + Duration::from_secs(8);
        let active = profile_path.join("DevToolsActivePort");
        let (port, endpoint) = loop {
            if let Ok(contents) = fs::read_to_string(&active) {
                let mut lines = contents.lines();
                if let (Some(port), Some(endpoint)) = (lines.next(), lines.next())
                    && let Ok(port) = port.parse::<u16>()
                {
                    break (port, endpoint.to_owned());
                }
            }
            let status = {
                let mut state = shared.0.lock().unwrap_or_else(|e| e.into_inner());
                match state.child.as_mut() {
                    Some(child) => child
                        .try_wait()
                        .map_err(|e| format!("Cannot inspect Chromium: {e}"))?,
                    None => return Err("Markdown renderer is shutting down.".into()),
                }
            };
            if let Some(status) = status {
                return Err(format!(
                    "Chromium exited during startup ({status}). Press m for source or o to open externally."
                ));
            }
            if Instant::now() >= deadline {
                return Err(
                    "Chromium did not start in time. Press m for source or o to open externally."
                        .into(),
                );
            }
            thread::sleep(Duration::from_millis(25));
        };
        let address = ("127.0.0.1", port)
            .to_socket_addrs()
            .map_err(|e| e.to_string())?
            .next()
            .ok_or("Invalid Chromium address")?;
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(3))
            .map_err(|e| format!("Cannot connect to Chromium: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        let url = format!("ws://127.0.0.1:{port}{endpoint}");
        let config = WebSocketConfig::default()
            .max_message_size(Some(128 * 1024 * 1024))
            .max_frame_size(Some(128 * 1024 * 1024));
        let (socket, _) =
            client_with_config(url.as_str(), MaybeTlsStream::Plain(stream), Some(config))
                .map_err(|e| format!("Chromium connection failed: {e}"))?;
        child.armed = false;
        let mut browser = Self {
            shared,
            socket,
            session: String::new(),
            next_id: 0,
            cache: None,
            document_height: 0,
        };
        let target = browser.call("Target.createTarget", json!({"url":"about:blank"}), false)?;
        let target_id = target["targetId"]
            .as_str()
            .ok_or("Chromium did not return a target")?
            .to_owned();
        let session = browser.call(
            "Target.attachToTarget",
            json!({"targetId":target_id,"flatten":true}),
            false,
        )?;
        browser.session = session["sessionId"]
            .as_str()
            .ok_or("Chromium did not return a session")?
            .to_owned();
        browser.call("Page.enable", json!({}), true)?;
        browser.call("Network.enable", json!({}), true)?;
        browser.call(
            "Network.setBlockedURLs",
            json!({"urls":["http://*","https://*","ws://*","wss://*","ftp://*","file://*"]}),
            true,
        )?;
        Ok(browser)
    }

    fn call(&mut self, method: &str, params: Value, session: bool) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let mut command = json!({"id": id, "method": method, "params": params});
        if session {
            command["sessionId"] = json!(self.session);
        }
        self.socket
            .send(Message::Text(command.to_string().into()))
            .map_err(|e| format!("Chromium send failed: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if Instant::now() >= deadline {
                return Err(format!("Chromium timed out during {method}"));
            }
            let message = self
                .socket
                .read()
                .map_err(|e| format!("Chromium response failed: {e}"))?;
            if let Message::Text(text) = message {
                let value: Value = serde_json::from_str(&text)
                    .map_err(|e| format!("Invalid Chromium response: {e}"))?;
                if value["id"].as_u64() != Some(id) {
                    continue;
                }
                if let Some(error) = value.get("error") {
                    return Err(format!("Chromium {method}: {error}"));
                }
                return Ok(value["result"].clone());
            }
        }
    }

    fn render(&mut self, request: &RenderRequest, generation: u64) -> Result<RenderedPage, String> {
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
            height: request.height,
            dark: request.dark,
        };
        if self.cache.as_ref() != Some(&key) {
            self.cache = None;
            let deadline = Instant::now() + Duration::from_secs(5);
            let shared = Arc::clone(&self.shared);
            let mut cancelled = || {
                let state = shared.0.lock().unwrap_or_else(|e| e.into_inner());
                state.shutdown || state.generation != generation || Instant::now() >= deadline
            };
            let document = markdown_html_with_budget(
                &request.path,
                &request.source,
                request.dark,
                32 * 1024 * 1024,
                &mut cancelled,
            )?;
            self.call("Emulation.setDeviceMetricsOverride", json!({"width":request.width,"height":request.height,"deviceScaleFactor":1,"mobile":false}), true)?;
            let tree = self.call("Page.getFrameTree", json!({}), true)?;
            let frame = tree["frameTree"]["frame"]["id"]
                .as_str()
                .ok_or("Chromium did not return a frame")?
                .to_owned();
            self.call(
                "Page.setDocumentContent",
                json!({"frameId":frame,"html":document}),
                true,
            )?;
            let measured = self.call("Runtime.evaluate", json!({"expression":"Promise.all([document.fonts.ready, ...Array.from(document.images, image => image.decode().catch(() => {}))]).then(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))).then(() => Math.max(document.documentElement.scrollHeight, document.body.scrollHeight))","awaitPromise":true,"returnByValue":true}), true)?;
            if !measured["exceptionDetails"].is_null() {
                return Err("Chromium could not measure the Markdown page.".into());
            }
            let height = measured["result"]["value"]
                .as_u64()
                .ok_or("Chromium did not return the page height")?;
            self.document_height =
                u32::try_from(height).map_err(|_| "Markdown page is too tall to render")?;
            self.cache = Some(key);
        }
        let max_scroll = self.document_height.saturating_sub(request.height);
        let scroll_y = request.scroll_y.min(max_scroll);
        let result = self.call("Page.captureScreenshot", json!({"format":"png","fromSurface":true,"captureBeyondViewport":true,"clip":{"x":0,"y":scroll_y,"width":request.width,"height":request.height,"scale":1}}), true)?;
        let data = result["data"]
            .as_str()
            .ok_or("Chromium did not return a screenshot")?;
        let bytes = STANDARD
            .decode(data)
            .map_err(|e| format!("Invalid Chromium screenshot: {e}"))?;
        Ok(RenderedPage {
            image: crate::media::image_from_png(bytes)?,
            scroll_y,
            max_scroll,
        })
    }
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn decode_path(value: &str) -> Option<PathBuf> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let pair = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(pair, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let text = std::str::from_utf8(&out).ok()?;
    if text.contains('\0')
        || text.contains('\\')
        || text.contains(':')
        || text.contains('?')
        || text.contains('#')
    {
        return None;
    }
    let path = PathBuf::from(text);
    if path.is_absolute() {
        return None;
    }
    Some(path)
}

// Resolve only to select an in-root path; open every resolved component without
// following symlinks, so replacement after canonicalization cannot escape.
fn open_local_image(parent: &Path, destination: &str) -> Option<fs::File> {
    let relative = decode_path(destination)?;
    let root = parent.canonicalize().ok()?;
    let dir = open_image_root(&root)?;
    let resolved = root.join(relative).canonicalize().ok()?;
    let relative = resolved.strip_prefix(&root).ok()?;
    open_beneath_image_root(dir, relative)
}

fn open_image_root(root: &Path) -> Option<std::os::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags, open, openat};
    use std::path::Component;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut components = root.components();
    if components.next()? != Component::RootDir {
        return None;
    }
    let mut dir = open("/", flags, Mode::empty()).ok()?;
    for component in components {
        let Component::Normal(name) = component else {
            return None;
        };
        dir = openat(&dir, name, flags, Mode::empty()).ok()?;
    }
    Some(dir)
}

#[cfg(test)]
fn open_resolved_image(root: &Path, relative: &Path) -> Option<fs::File> {
    open_beneath_image_root(open_image_root(root)?, relative)
}

fn open_beneath_image_root(mut dir: std::os::fd::OwnedFd, relative: &Path) -> Option<fs::File> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::path::Component;
    let components: Vec<_> = relative.components().collect();
    let (last, directories) = components.split_last()?;
    let Component::Normal(last) = last else {
        return None;
    };
    for component in directories {
        let Component::Normal(name) = component else {
            return None;
        };
        dir = openat(
            &dir,
            *name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .ok()?;
    }
    let file = openat(
        &dir,
        *last,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    Some(fs::File::from(file))
}

struct Asset {
    url: String,
    pixels: u64,
}

struct Assets {
    cache: HashMap<(u64, u64), Option<Asset>>,
    source_bytes: u64,
    decoded_pixels: u64,
    browser_pixels: u64,
    occurrences: usize,
}

impl Assets {
    fn new() -> Self {
        Self {
            cache: HashMap::new(),
            source_bytes: 32 * 1024 * 1024,
            decoded_pixels: 32 * 1024 * 1024,
            browser_pixels: 32 * 1024 * 1024,
            occurrences: 128,
        }
    }

    fn load(&mut self, parent: &Path, destination: &str) -> Option<&Asset> {
        use std::os::unix::fs::MetadataExt;
        if self.occurrences == 0 {
            return None;
        }
        self.occurrences -= 1;
        let file = open_local_image(parent, destination)?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() {
            return None;
        }
        let identity = (metadata.dev(), metadata.ino());
        if !self.cache.contains_key(&identity) {
            let image = self.decode(file, metadata.len());
            self.cache.insert(identity, image);
        }
        let asset = self.cache.get(&identity)?.as_ref()?;
        if asset.pixels > self.browser_pixels {
            return None;
        }
        self.browser_pixels -= asset.pixels;
        Some(asset)
    }

    fn decode(&mut self, file: fs::File, size: u64) -> Option<Asset> {
        if size > 16 * 1024 * 1024 || size > self.source_bytes {
            return None;
        }
        self.source_bytes -= size;
        let mut data = Vec::new();
        file.take(size + 1).read_to_end(&mut data).ok()?;
        if data.len() as u64 != size {
            return None;
        }
        let format = image::guess_format(&data).ok()?;
        if !matches!(
            format,
            image::ImageFormat::Png
                | image::ImageFormat::Jpeg
                | image::ImageFormat::Gif
                | image::ImageFormat::WebP
                | image::ImageFormat::Bmp
        ) {
            return None;
        }
        let reader = image::ImageReader::with_format(BufReader::new(Cursor::new(&data)), format);
        let (width, height) = reader.into_dimensions().ok()?;
        let pixels = u64::from(width) * u64::from(height);
        if width == 0
            || height == 0
            || width > 16_384
            || height > 16_384
            || pixels > 16 * 1024 * 1024
            || pixels > self.decoded_pixels
        {
            return None;
        }
        self.decoded_pixels -= pixels;
        let mut reader = image::ImageReader::with_format(Cursor::new(&data), format);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(16_384);
        limits.max_image_height = Some(16_384);
        limits.max_alloc = Some(16 * 1024 * 1024 * 4);
        reader.limits(limits);
        let mut decoder = reader.into_decoder().ok()?;
        let orientation = decoder.orientation().ok()?;
        let mut image = image::DynamicImage::from_decoder(decoder).ok()?;
        image.apply_orientation(orientation);
        if image.width() > 2048 || image.height() > 2048 {
            image = image.thumbnail(2048, 2048);
        }
        let pixels = u64::from(image.width()) * u64::from(image.height());
        if pixels > self.browser_pixels {
            return None;
        }
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).ok()?;
        Some(Asset {
            url: format!(
                "data:image/png;base64,{}",
                STANDARD.encode(png.into_inner())
            ),
            pixels,
        })
    }
}

#[cfg(test)]
fn markdown_html(path: &Path, source: &str, dark: bool) -> String {
    markdown_html_with_budget(path, source, dark, 32 * 1024 * 1024, &mut || false).unwrap()
}

fn markdown_html_with_budget(
    path: &Path,
    source: &str,
    dark: bool,
    mut remaining_image_bytes: usize,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<String, String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut options = Options::empty();
    options
        .insert(Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS);
    let mut events = Vec::new();
    let mut images = Assets::new();
    // Count every occurrence, not just unique files: each becomes HTML bytes.
    let mut iter = Parser::new_ext(source, options).peekable();
    while let Some(event) = iter.next() {
        if cancelled() {
            return Err("Markdown rendering cancelled.".into());
        }
        match event {
            Event::Html(text) | Event::InlineHtml(text) => events.push(Event::Text(text)),
            Event::Start(Tag::Link {
                link_type,
                title,
                id,
                ..
            }) => events.push(Event::Start(Tag::Link {
                link_type,
                dest_url: CowStr::from("#"),
                title,
                id,
            })),
            Event::Start(Tag::Image { dest_url, .. }) => {
                let mut depth = 1;
                let mut alt = String::new();
                for item in iter.by_ref() {
                    if cancelled() {
                        return Err("Markdown rendering cancelled.".into());
                    }
                    match item {
                        Event::Start(Tag::Image { .. }) => depth += 1,
                        Event::End(TagEnd::Image) => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        Event::Text(t) | Event::Code(t) => alt.push_str(&t),
                        Event::SoftBreak | Event::HardBreak => alt.push(' '),
                        _ => {}
                    }
                }
                let safe_alt = escape_html(&alt);
                let src = if remaining_image_bytes > 0 {
                    images.load(parent, &dest_url)
                } else {
                    None
                };
                if cancelled() {
                    return Err("Markdown rendering cancelled.".into());
                }
                let tag = if let Some(src) =
                    src.filter(|src| src.url.len() <= remaining_image_bytes)
                {
                    remaining_image_bytes -= src.url.len();
                    format!("<img src=\"{}\" alt=\"{safe_alt}\">", src.url)
                } else {
                    format!(
                        "<span class=\"missing-image\">[Image unavailable or image budget exceeded: {safe_alt}]</span>"
                    )
                };
                events.push(Event::Html(CowStr::from(tag)));
            }
            other => events.push(other),
        }
    }
    let mut body = String::new();
    html::push_html(&mut body, events.into_iter());
    let (background, foreground, muted, line, code, link) = if dark {
        (
            "#0d1117", "#e6edf3", "#9da7b3", "#30363d", "#161b22", "#79c0ff",
        )
    } else {
        (
            "#ffffff", "#24292f", "#57606a", "#d0d7de", "#f6f8fa", "#0969da",
        )
    };
    Ok(format!(
        r##"<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'"><style>
:root {{ color-scheme: {}; }} * {{ box-sizing: border-box; }} html,body {{ margin:0; padding:0; background:{background}; color:{foreground}; }} body {{ font:16px/1.55 system-ui, sans-serif; overflow-wrap:anywhere; padding:24px 32px 48px; }} h1,h2,h3,h4,h5,h6 {{ line-height:1.25; margin:1.4em 0 .6em; }} h1 {{ font-size:2em; border-bottom:1px solid {line}; padding-bottom:.3em; }} h2 {{ font-size:1.5em; border-bottom:1px solid {line}; padding-bottom:.3em; }} h3 {{ font-size:1.25em; }} p,ul,ol,pre,blockquote,table {{ margin:0 0 1em; }} ul,ol {{ padding-left:2em; }} li+li {{ margin-top:.25em; }} li p {{ margin-bottom:.5em; }} a {{ color:{link}; }} blockquote {{ margin-left:0; padding:0 1em; color:{muted}; border-left:4px solid {line}; }} pre {{ overflow:auto; background:{code}; padding:1em; border-radius:6px; }} code {{ font: .9em ui-monospace, monospace; background:{code}; padding:.15em .3em; border-radius:4px; }} pre code {{ padding:0; background:none; }} table {{ border-collapse:collapse; display:block; max-width:100%; overflow:auto; }} th,td {{ padding:.4em .8em; border:1px solid {line}; }} tr:nth-child(even) {{ background:{code}; }} img {{ max-width:100%; height:auto; }} hr {{ border:0; border-top:1px solid {line}; margin:1.5em 0; }} .missing-image {{ color:{muted}; }} input {{ pointer-events:none; }} @media(max-width:500px) {{ body {{ padding:16px; }} }}
</style></head><body>{body}</body></html>"##,
        if dark { "dark" } else { "light" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_is_semantic_and_untrusted_html_is_text() {
        let html = markdown_html(
            Path::new("/tmp/test.md"),
            "# Heading\n\n<script>alert(1)</script>\n\n[a](https://example.com)\n\n| A | B |\n|---|---|\n| 1 | 2 |",
            false,
        );
        assert!(html.contains("<h1>Heading</h1>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("href=\"https://"));
        assert!(html.contains("<table>"));
    }

    #[test]
    fn image_paths_are_confined_and_remote_images_are_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]));
        image.save(dir.path().join("test.png")).unwrap();
        image.save(outside.path().join("outside.png")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("outside.png"),
            dir.path().join("escape.png"),
        )
        .unwrap();
        let mut assets = Assets::new();
        assert!(
            assets
                .load(dir.path(), "test.png")
                .unwrap()
                .url
                .starts_with("data:image/png;base64,")
        );
        let used = assets.source_bytes;
        assert!(assets.load(dir.path(), "%74est.png").is_some());
        assert_eq!(assets.source_bytes, used);
        assert!(open_local_image(dir.path(), "escape.png").is_none());
        assert!(open_local_image(dir.path(), "https://example.com/a.png").is_none());
        assert!(open_local_image(dir.path(), "%2e%2e/outside.png").is_none());
        let html = markdown_html(
            &dir.path().join("test.md"),
            "![safe](test.png) ![blocked](https://example.com/a.png)",
            false,
        );
        assert!(html.contains("data:image/png;base64,"));
        assert!(html.contains("Image unavailable or image budget exceeded: blocked"));
        let url_length = assets.load(dir.path(), "test.png").unwrap().url.len();
        let limited = markdown_html_with_budget(
            &dir.path().join("test.md"),
            "![first](test.png) ![second](test.png)",
            false,
            url_length,
            &mut || false,
        )
        .unwrap();
        assert_eq!(limited.matches("data:image/png;base64,").count(), 1);
        assert!(limited.contains("image budget exceeded: second"));
        assert!(!html.contains("https://example.com"));
        fs::remove_file(dir.path().join("test.png")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("outside.png"),
            dir.path().join("test.png"),
        )
        .unwrap();
        assert!(open_resolved_image(dir.path(), Path::new("test.png")).is_none());
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::remove_dir(dir.path().join("sub")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("sub")).unwrap();
        assert!(open_resolved_image(dir.path(), Path::new("sub/outside.png")).is_none());
        let root = rustix::fs::open(
            dir.path(),
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        rustix::fs::mkfifoat(
            &root,
            "fifo",
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        let fifo = open_resolved_image(dir.path(), Path::new("fifo")).unwrap();
        assert!(!fifo.metadata().unwrap().is_file());
    }

    #[test]
    fn conversion_observes_cancellation_and_asset_limits() {
        let dir = tempfile::tempdir().unwrap();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]))
            .save(dir.path().join("image.png"))
            .unwrap();
        let mut assets = Assets::new();
        assets.decoded_pixels = 3;
        assert!(assets.load(dir.path(), "image.png").is_none());
        let mut calls = 0;
        let result = markdown_html_with_budget(
            &dir.path().join("a.md"),
            "# one\n\n![a](image.png)",
            false,
            1024,
            &mut || {
                calls += 1;
                calls > 1
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn cancel_discards_queued_and_ready_results() {
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let renderer = MarkdownRenderer {
            shared: Arc::clone(&shared),
        };
        let request = RenderRequest {
            path: PathBuf::from("test.md"),
            source: Arc::from("test"),
            width: 100,
            height: 100,
            scroll_y: 0,
            dark: false,
        };
        let first = renderer.request(request.clone());
        let second = renderer.request(request);
        assert_ne!(first, second);
        assert_eq!(shared.0.lock().unwrap().queued.as_ref().unwrap().0, second);
        {
            let mut state = shared.0.lock().unwrap();
            state.working = true;
            state.finish(first, Err("stale".into()));
            assert!(state.ready.is_none());
            state.queued = None;
            state.finish(second, Err("latest".into()));
        }
        assert!(renderer.is_pending());
        assert_eq!(renderer.poll().unwrap().1.unwrap_err(), "latest");
        renderer.request(RenderRequest {
            path: PathBuf::from("other.md"),
            source: Arc::from("other"),
            width: 100,
            height: 100,
            scroll_y: 0,
            dark: false,
        });
        renderer.cancel();
        assert!(!renderer.is_pending());
        assert!(renderer.poll().is_none());
    }
    #[test]
    fn chromium_captures_top_end_home_and_resize() {
        if chromium_path().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        image::RgbaImage::from_pixel(40, 40, image::Rgba([240, 10, 10, 255]))
            .save(dir.path().join("marker.png"))
            .unwrap();
        let source = format!(
            "# TOP\n\n![marker](marker.png)\n\n{}\n\n# BOTTOM",
            "A long paragraph with visible text.\n\n".repeat(100)
        );
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let mut browser = Browser::launch(shared).unwrap();
        let mut request = RenderRequest {
            path: dir.path().join("page.md"),
            source: Arc::from(source),
            width: 600,
            height: 400,
            scroll_y: 0,
            dark: false,
        };
        let decode = |page: &RenderedPage| {
            image::load_from_memory(&STANDARD.decode(page.image.encoded.as_ref()).unwrap())
                .unwrap()
                .to_rgba8()
        };
        let top = browser.render(&request, 0).unwrap();
        assert!(top.max_scroll > 0);
        let top_image = decode(&top);
        assert!(
            top_image
                .pixels()
                .any(|p| p[0] > 200 && p[1] < 40 && p[2] < 40)
        );
        request.scroll_y = u32::MAX;
        let end = browser.render(&request, 0).unwrap();
        assert_eq!(end.scroll_y, end.max_scroll);
        let end_image = decode(&end);
        assert_ne!(top_image, end_image);
        request.scroll_y = 0;
        let home = decode(&browser.render(&request, 0).unwrap());
        assert_eq!(top_image, home);
        request.width = 800;
        let resized = decode(&browser.render(&request, 0).unwrap());
        assert_eq!(resized.dimensions(), (800, 400));
        assert!(
            resized
                .pixels()
                .any(|p| p[0] > 200 && p[1] < 40 && p[2] < 40)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn renderer_drop_kills_and_reaps_registered_child() {
        let child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id();
        let profile = tempfile::tempdir().unwrap();
        let profile_path = profile.path().to_owned();
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        {
            let mut state = shared.0.lock().unwrap();
            state.child = Some(child);
            state.profile = Some(profile);
        }
        let renderer = MarkdownRenderer {
            shared: Arc::clone(&shared),
        };
        drop(renderer);
        let state = shared.0.lock().unwrap();
        assert!(state.shutdown);
        assert!(state.child.is_none());
        assert!(state.profile.is_none());
        assert!(!profile_path.exists());
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
}
