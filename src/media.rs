//! Bounded image decoding for terminals that support the Kitty graphics protocol.

use base64::Engine;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::Arc;

const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_DIMENSION: u32 = 16_384;
const MAX_TRANSMITTED_DIMENSION: u32 = 2048;

/// An immutable, shared, encoded first frame ready for the Kitty graphics protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImagePreview {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) encoded: Arc<str>,
}

pub fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ["png", "jpg", "jpeg", "gif", "webp", "bmp"]
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

fn terminal_support(
    term: &str,
    program: &str,
    tmux: bool,
    screen: bool,
    zellij: bool,
) -> Result<(), String> {
    if tmux || screen || zellij || term.starts_with("screen") || term.starts_with("tmux") {
        return Err("Image previews are unavailable inside terminal multiplexers. Press o to open externally.".into());
    }
    if term == "xterm-kitty" || term == "xterm-ghostty" || program.eq_ignore_ascii_case("ghostty") {
        Ok(())
    } else {
        Err("Image previews require Kitty or Ghostty without a terminal multiplexer. Press o to open externally.".into())
    }
}

pub fn load_image(path: &Path) -> Result<ImagePreview, String> {
    terminal_support(
        &std::env::var("TERM").unwrap_or_default(),
        &std::env::var("TERM_PROGRAM").unwrap_or_default(),
        std::env::var_os("TMUX").is_some(),
        std::env::var_os("STY").is_some(),
        std::env::var_os("ZELLIJ").is_some(),
    )?;
    decode_image(path)
}

fn decode_image(path: &Path) -> Result<ImagePreview, String> {
    // Inspect before opening: opening a FIFO for reading can wait indefinitely.
    let metadata = std::fs::metadata(path)
        .map_err(|e| format!("Cannot inspect image: {e}. Press o to open externally."))?;
    if !metadata.is_file() {
        return Err("Image is not a regular file. Press o to open externally.".into());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(
            "Image file exceeds the 16 MiB preview limit. Press o to open externally.".into(),
        );
    }
    let file = File::open(path)
        .map_err(|e| format!("Cannot open image: {e}. Press o to open externally."))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("Cannot inspect image: {e}. Press o to open externally."))?;
    if !metadata.is_file() {
        return Err("Image is not a regular file. Press o to open externally.".into());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(
            "Image file exceeds the 16 MiB preview limit. Press o to open externally.".into(),
        );
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Cannot read image: {e}. Press o to open externally."))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(
            "Image file exceeds the 16 MiB preview limit. Press o to open externally.".into(),
        );
    }
    let reader = || ImageReader::new(Cursor::new(bytes.as_slice())).with_guessed_format();
    let (width, height) = reader()
        .map_err(|e| format!("Cannot identify image: {e}. Press o to open externally."))?
        .into_dimensions()
        .map_err(|e| format!("Cannot decode image dimensions: {e}. Press o to open externally."))?;
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return Err(
            "Image dimensions exceed the preview limit. Press o to open externally.".into(),
        );
    }
    let mut reader =
        reader().map_err(|e| format!("Cannot identify image: {e}. Press o to open externally."))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_PIXELS * 4);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("Cannot decode image: {e}. Press o to open externally."))?;
    let orientation = decoder
        .orientation()
        .map_err(|e| format!("Cannot read image orientation: {e}. Press o to open externally."))?;
    let mut image = DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("Cannot decode image: {e}. Press o to open externally."))?;
    image.apply_orientation(orientation);
    if image.width() > MAX_TRANSMITTED_DIMENSION || image.height() > MAX_TRANSMITTED_DIMENSION {
        image = image.thumbnail(MAX_TRANSMITTED_DIMENSION, MAX_TRANSMITTED_DIMENSION);
    }
    let (width, height) = (image.width(), image.height());
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| format!("Cannot encode image: {e}. Press o to open externally."))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    Ok(ImagePreview {
        width,
        height,
        encoded: Arc::from(encoded.into_boxed_str()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_multiplexers_even_with_kitty_term() {
        assert!(terminal_support("xterm-kitty", "", true, false, false).is_err());
        assert!(terminal_support("screen-256color", "ghostty", false, false, false).is_err());
        assert!(terminal_support("xterm-kitty", "", false, false, true).is_err());
        assert!(terminal_support("xterm-kitty", "", false, false, false).is_ok());
        assert!(terminal_support("xterm-256color", "ghostty", false, false, false).is_ok());
        assert!(terminal_support("xterm-256color", "", false, false, false).is_err());
    }

    #[test]
    fn decodes_pixels_and_recognizes_case_insensitive_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pixel.PNG");
        image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]))
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();
        let image = decode_image(&path).unwrap();
        assert_eq!((image.width, image.height), (1, 1));
        let png = base64::engine::general_purpose::STANDARD
            .decode(&*image.encoded)
            .unwrap();
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
        assert_eq!(decoded.to_rgba8().get_pixel(0, 0).0, [1, 2, 3, 255]);
        assert!(is_image_path(&path));
        assert!(!is_image_path(Path::new("movie.mp4")));
    }

    #[test]
    fn downscales_large_images_before_encoding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.png");
        image::RgbaImage::from_pixel(4096, 1024, image::Rgba([1, 2, 3, 255]))
            .save(&path)
            .unwrap();
        let image = decode_image(&path).unwrap();
        assert_eq!((image.width, image.height), (2048, 512));
    }

    #[test]
    fn applies_jpeg_exif_orientation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oriented.jpg");
        image::RgbImage::from_pixel(3, 2, image::Rgb([20, 40, 60]))
            .save(&path)
            .unwrap();
        let jpeg = std::fs::read(&path).unwrap();
        // APP1 Exif with a little-endian TIFF orientation tag set to 6 (90 degrees).
        let exif = [
            0xff, 0xe1, 0x00, 0x22, b'E', b'x', b'i', b'f', 0, 0, b'I', b'I', 0x2a, 0, 8, 0, 0, 0,
            1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
        ];
        let mut tagged = jpeg[..2].to_vec();
        tagged.extend_from_slice(&exif);
        tagged.extend_from_slice(&jpeg[2..]);
        std::fs::write(&path, tagged).unwrap();
        let image = decode_image(&path).unwrap();
        assert_eq!((image.width, image.height), (2, 3));
    }

    #[test]
    fn rejects_oversized_file_and_invalid_image() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            decode_image(directory.path())
                .unwrap_err()
                .contains("not a regular file")
        );
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(decode_image(file.path()).unwrap_err().contains("16 MiB"));
        file.as_file().set_len(0).unwrap();
        assert!(decode_image(file.path()).is_err());
    }
}
