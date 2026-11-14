//! Bounded image decoding and PNG preparation for the graphics transport.

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
            ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg"]
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

pub(crate) fn is_svg_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
}

pub(crate) fn load_svg(source: &str) -> Result<ImagePreview, String> {
    image_from_png(crate::svg::Svg::parse(source)?.render()?)
}

pub(crate) fn image_from_png(bytes: Vec<u8>) -> Result<ImagePreview, String> {
    let (width, height) = ImageReader::with_format(Cursor::new(&bytes), image::ImageFormat::Png)
        .into_dimensions()
        .map_err(|error| format!("Cannot read rendered document image: {error}"))?;
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return Err("Rendered document viewport exceeds the image size limit.".into());
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(ImagePreview {
        width,
        height,
        encoded: Arc::from(encoded.into_boxed_str()),
    })
}

pub fn load_image(path: &Path) -> Result<ImagePreview, String> {
    if is_svg_path(path) {
        let metadata =
            std::fs::metadata(path).map_err(|error| format!("Cannot inspect SVG: {error}"))?;
        if !metadata.is_file() {
            return Err("SVG is not a regular file.".into());
        }
        let file = File::open(path).map_err(|error| format!("Cannot open SVG: {error}"))?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("Cannot inspect SVG: {error}"))?;
        if !metadata.is_file() || metadata.len() > crate::svg::MAX_SOURCE_BYTES {
            return Err("SVG must be a regular file within the 1 MiB source limit.".into());
        }
        let mut source = String::new();
        file.take(crate::svg::MAX_SOURCE_BYTES + 1)
            .read_to_string(&mut source)
            .map_err(|error| format!("Cannot read SVG UTF-8 source: {error}"))?;
        return load_svg(&source);
    }
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
    fn public_decoder_preserves_pixels_without_terminal_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pixel.PNG");
        image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]))
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();
        let image = load_image(&path).unwrap();
        assert_eq!(image, load_image(&path).unwrap());
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
        let image = load_image(&path).unwrap();
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
        let image = load_image(&path).unwrap();
        assert_eq!((image.width, image.height), (2, 3));
    }

    #[test]
    fn rejects_oversized_file_and_invalid_image() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            load_image(directory.path())
                .unwrap_err()
                .contains("not a regular file")
        );
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(load_image(file.path()).unwrap_err().contains("16 MiB"));
        file.as_file().set_len(0).unwrap();
        assert!(load_image(file.path()).is_err());
    }

    #[test]
    fn standalone_svg_uses_png_transport_and_rejects_svgz_and_large_sources() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vector.SvG");
        std::fs::write(&path, r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="2"><rect width="2" height="2" fill="red"/></svg>"#).unwrap();
        let preview = load_image(&path).unwrap();
        assert!(is_image_path(&path));
        assert_eq!((preview.width, preview.height), (4, 2));
        let png = base64::engine::general_purpose::STANDARD
            .decode(&*preview.encoded)
            .unwrap();
        let image = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(image.get_pixel(3, 0).0, [0, 0, 0, 0]);
        assert!(!is_image_path(Path::new("vector.svgz")));
        File::create(&path)
            .unwrap()
            .set_len(crate::svg::MAX_SOURCE_BYTES + 1)
            .unwrap();
        assert!(load_image(&path).unwrap_err().contains("1 MiB"));
        std::fs::write(&path, b"\x1f\x8bnot-an-xml-document").unwrap();
        assert!(load_image(&path).is_err());
    }
}
