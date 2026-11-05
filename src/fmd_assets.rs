//! Confined, bounded image preparation for the Markdown renderer.

use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, Cursor, Read};
use std::path::{Component, Path, PathBuf};

use franken_markdown::{Block, Document, Inline, PdfImageAsset};
use image::ImageDecoder;

const BUDGET: u64 = 32 * 1024 * 1024;

pub(crate) fn prepare(
    doc: &mut Document,
    path: &Path,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<Vec<PdfImageAsset>, String> {
    check_cancelled(cancelled)?;
    let mut assets = Assets::new(path.parent().unwrap_or_else(|| Path::new(".")));
    assets.blocks(&mut doc.blocks, cancelled)?;
    Ok(assets.output)
}

struct Image {
    png: Vec<u8>,
    pixels: u64,
}

struct Assets<'a> {
    parent: &'a Path,
    cache: HashMap<(u64, u64), Option<Image>>,
    source_bytes: u64,
    decoded_pixels: u64,
    rendered_pixels: u64,
    encoded_bytes: u64,
    occurrences: usize,
    output: Vec<PdfImageAsset>,
}

impl<'a> Assets<'a> {
    fn new(parent: &'a Path) -> Self {
        Self {
            parent,
            cache: HashMap::new(),
            source_bytes: BUDGET,
            decoded_pixels: BUDGET,
            rendered_pixels: BUDGET,
            encoded_bytes: BUDGET,
            occurrences: 128,
            output: Vec::new(),
        }
    }

    fn blocks(
        &mut self,
        blocks: &mut [Block],
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), String> {
        for block in blocks {
            check_cancelled(cancelled)?;
            match block {
                Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
                    self.inlines(inlines, cancelled)?
                }
                Block::BlockQuote(blocks) | Block::FootnoteDefinition { blocks, .. } => {
                    self.blocks(blocks, cancelled)?
                }
                Block::List(list) => {
                    for item in &mut list.items {
                        self.blocks(&mut item.blocks, cancelled)?;
                    }
                }
                Block::Table(table) => {
                    for cell in &mut table.head {
                        self.inlines(cell, cancelled)?;
                    }
                    for row in &mut table.rows {
                        for cell in row {
                            self.inlines(cell, cancelled)?;
                        }
                    }
                }
                Block::DefinitionList(items) => {
                    for item in items {
                        for inlines in item.terms.iter_mut().chain(&mut item.definitions) {
                            self.inlines(inlines, cancelled)?;
                        }
                    }
                }
                Block::HtmlBlock(html) => {
                    *block = Block::Paragraph(vec![Inline::Text(std::mem::take(html))]);
                }
                Block::CodeBlock { .. }
                | Block::ThematicBreak
                | Block::MathBlock(_)
                | Block::PageBreak => {}
            }
        }
        Ok(())
    }

    fn inlines(
        &mut self,
        inlines: &mut [Inline],
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), String> {
        for inline in inlines {
            check_cancelled(cancelled)?;
            match inline {
                Inline::Emphasis(content)
                | Inline::Strong(content)
                | Inline::Strikethrough(content)
                | Inline::Link { content, .. } => self.inlines(content, cancelled)?,
                Inline::Image { dest, alt, .. } => {
                    if let Some(key) = self.load(dest) {
                        *dest = key;
                    } else {
                        *inline = Inline::Text(format!(
                            "[Image unavailable or image budget exceeded: {alt}]"
                        ));
                    }
                }
                Inline::Html(html) => *inline = Inline::Text(std::mem::take(html)),
                Inline::Text(_)
                | Inline::Code(_)
                | Inline::SoftBreak
                | Inline::HardBreak
                | Inline::FootnoteRef { .. }
                | Inline::Math(_)
                | Inline::DisplayMath(_) => {}
            }
        }
        Ok(())
    }

    fn load(&mut self, destination: &str) -> Option<String> {
        use std::os::unix::fs::MetadataExt;
        if self.occurrences == 0 {
            return None;
        }
        self.occurrences -= 1;
        let file = open_local_image(self.parent, destination)?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() {
            return None;
        }
        let identity = (metadata.dev(), metadata.ino());
        if !self.cache.contains_key(&identity) {
            let image = self.decode(file, metadata.len());
            self.cache.insert(identity, image);
        }
        let image = self.cache.get(&identity)?.as_ref()?;
        // Charge the original data-URL size for every occurrence, including repeats.
        let encoded = 22 + 4 * (image.png.len() as u64).div_ceil(3);
        if image.pixels > self.rendered_pixels || encoded > self.encoded_bytes {
            return None;
        }
        self.rendered_pixels -= image.pixels;
        self.encoded_bytes -= encoded;
        let key = format!("de-safe-image-{}", self.output.len());
        self.output
            .push(PdfImageAsset::new(key.clone(), image.png.clone()));
        Some(key)
    }

    fn decode(&mut self, file: fs::File, size: u64) -> Option<Image> {
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
        let (width, height) =
            image::ImageReader::with_format(BufReader::new(Cursor::new(&data)), format)
                .into_dimensions()
                .ok()?;
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
        if pixels > self.rendered_pixels {
            return None;
        }
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).ok()?;
        Some(Image {
            png: png.into_inner(),
            pixels,
        })
    }
}

fn check_cancelled(cancelled: &mut dyn FnMut() -> bool) -> Result<(), String> {
    if cancelled() {
        Err("Markdown rendering cancelled.".into())
    } else {
        Ok(())
    }
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
    if text.contains(['\0', '\\', ':', '?', '#']) {
        return None;
    }
    let path = PathBuf::from(text);
    if path.is_absolute() {
        return None;
    }
    Some(path)
}

// Resolve inside the root, then open each component without following symlinks.
fn open_local_image(parent: &Path, destination: &str) -> Option<fs::File> {
    use rustix::fs::{Mode, OFlags, open, openat};
    let relative = decode_path(destination)?;
    let root = parent.canonicalize().ok()?;
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
    let resolved = root.join(relative).canonicalize().ok()?;
    let relative = resolved.strip_prefix(&root).ok()?;
    let components: Vec<_> = relative.components().collect();
    let (last, directories) = components.split_last()?;
    let Component::Normal(last) = last else {
        return None;
    };
    for component in directories {
        let Component::Normal(name) = component else {
            return None;
        };
        dir = openat(&dir, *name, flags, Mode::empty()).ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn image(dest: &str) -> Inline {
        Inline::Image {
            dest: dest.into(),
            title: None,
            alt: "description".into(),
        }
    }

    #[test]
    fn confines_images_and_disarms_html() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]))
            .save(dir.path().join("image.png"))
            .unwrap();
        image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]))
            .save(outside.path().join("escape.png"))
            .unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("escape.png"),
            dir.path().join("escape.png"),
        )
        .unwrap();
        let mut doc = Document {
            blocks: vec![
                Block::Paragraph(vec![
                    image("image.png"),
                    image("%69mage.png"),
                    image("https://example.com/a.png"),
                    image("data:image/png;base64,AA=="),
                    image("%2e%2e/escape.png"),
                    image("escape.png"),
                    Inline::Html("<script>".into()),
                ]),
                Block::HtmlBlock("<img src=x>".into()),
            ],
        };
        let output = prepare(&mut doc, &dir.path().join("test.md"), &mut || false).unwrap();
        assert_eq!(output.len(), 2);
        assert_ne!(output[0].destination, output[1].destination);
        assert!(output[0].bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!()
        };
        assert!(
            inlines[2..6]
                .iter()
                .all(|inline| matches!(inline, Inline::Text(s) if s.contains("Image unavailable")))
        );
        assert!(matches!(&inlines[6], Inline::Text(s) if s == "<script>"));
        assert!(
            matches!(&doc.blocks[1], Block::Paragraph(v) if matches!(&v[0], Inline::Text(s) if s == "<img src=x>"))
        );
    }

    #[test]
    fn traverses_nested_content() {
        use franken_markdown::{Align, DefinitionItem, List, ListItem, Table};
        let dir = tempfile::tempdir().unwrap();
        image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]))
            .save(dir.path().join("image.png"))
            .unwrap();
        let mut doc = Document {
            blocks: vec![
                Block::BlockQuote(vec![Block::List(List {
                    ordered: false,
                    start: 1,
                    tight: true,
                    items: vec![ListItem {
                        task: None,
                        blocks: vec![Block::Paragraph(vec![Inline::Strong(vec![Inline::Link {
                            dest: "link".into(),
                            title: None,
                            content: vec![image("image.png")],
                        }])])],
                    }],
                })]),
                Block::Table(Table {
                    align: vec![Align::None],
                    head: vec![vec![image("image.png")]],
                    rows: vec![vec![vec![image("image.png")]]],
                }),
                Block::FootnoteDefinition {
                    id: "note".into(),
                    blocks: vec![Block::Paragraph(vec![image("image.png")])],
                },
                Block::DefinitionList(vec![DefinitionItem {
                    terms: vec![vec![image("image.png")]],
                    definitions: vec![vec![image("image.png")]],
                }]),
            ],
        };
        let output = prepare(&mut doc, &dir.path().join("test.md"), &mut || false).unwrap();
        assert_eq!(output.len(), 6);
        for (index, asset) in output.iter().enumerate() {
            assert_eq!(asset.destination, format!("de-safe-image-{index}"));
        }
    }
    #[test]
    fn enforces_each_budget_and_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]))
            .save(dir.path().join("image.png"))
            .unwrap();
        let mut assets = Assets::new(dir.path());
        assets.source_bytes = 0;
        assert!(assets.load("image.png").is_none());
        let mut assets = Assets::new(dir.path());
        assets.decoded_pixels = 3;
        assert!(assets.load("image.png").is_none());
        let mut assets = Assets::new(dir.path());
        assets.rendered_pixels = 4;
        assert!(assets.load("image.png").is_some());
        assert!(assets.load("image.png").is_none());
        let mut assets = Assets::new(dir.path());
        assets.encoded_bytes = 0;
        assert!(assets.load("image.png").is_none());
        let mut assets = Assets::new(dir.path());
        assets.occurrences = 1;
        assert!(assets.load("https://example.com/a.png").is_none());
        assert!(assets.load("image.png").is_none());
        let mut doc = Document {
            blocks: vec![Block::Paragraph(vec![image("image.png")])],
        };
        assert!(prepare(&mut doc, &dir.path().join("test.md"), &mut || true).is_err());
    }
}
