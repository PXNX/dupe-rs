use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::codecs::webp::WebPEncoder;
use image::{ColorType, DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader};
use std::path::Path;

/// What lossless image re-encoding should produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageTarget {
    /// Lossless WebP: usually the smallest. Changes the extension to `.webp`.
    LosslessWebp,
    /// PNG at maximum compression. Keeps `.png` files `.png`.
    OptimizedPng,
}

impl ImageTarget {
    pub fn label(self) -> &'static str {
        match self {
            ImageTarget::LosslessWebp => "Lossless WebP",
            ImageTarget::OptimizedPng => "Max-compression PNG",
        }
    }
}

/// Extensions of image formats stored losslessly, the only ones a lossless
/// re-encode can shrink. JPEG and lossy WebP aren't here: decoding and
/// re-encoding them losslessly always makes them bigger.
pub const LOSSLESS_IMAGE_EXTENSIONS: &[&str] = &[
    "png", "bmp", "tif", "tiff", "tga", "ppm", "pgm", "pbm", "pnm", "qoi",
];

pub fn is_reencodable_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| LOSSLESS_IMAGE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
}

/// Re-encodes the image at `path` losslessly, returning the new bytes and
/// their extension. The result is decoded again and compared pixel for pixel
/// with the original before being returned; ICC profiles and EXIF data are
/// carried over, and a file whose metadata can't be kept is refused rather
/// than silently stripped.
pub fn reencode_image(path: &Path, target: ImageTarget) -> Result<(Vec<u8>, &'static str), String> {
    let mut decoder = ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string())?;
    let icc = decoder.icc_profile().ok().flatten();
    let exif = decoder.exif_metadata().ok().flatten();
    let original = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;

    // WebP lossless only stores 8 bits per channel; deeper images stay PNG.
    let eight_bit = matches!(
        original.color(),
        ColorType::L8 | ColorType::La8 | ColorType::Rgb8 | ColorType::Rgba8
    );
    let (bytes, ext, format) = if target == ImageTarget::LosslessWebp && eight_bit {
        (
            encode_webp(&original, icc, exif)?,
            "webp",
            ImageFormat::WebP,
        )
    } else {
        (encode_png(&original, icc, exif)?, "png", ImageFormat::Png)
    };

    let decoded = image::load_from_memory_with_format(&bytes, format).map_err(|e| e.to_string())?;
    if !same_pixels(&original, &decoded) {
        return Err("re-encoded pixels didn't match the original; left untouched".into());
    }
    Ok((bytes, ext))
}

fn encode_webp(
    img: &DynamicImage,
    icc: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut encoder = WebPEncoder::new_lossless(&mut out);
    apply_metadata(&mut encoder, icc, exif)?;
    encoder
        .write_image(
            img.as_bytes(),
            img.width(),
            img.height(),
            img.color().into(),
        )
        .map_err(|e| e.to_string())?;
    Ok(out)
}

fn encode_png(
    img: &DynamicImage,
    icc: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut encoder =
        PngEncoder::new_with_quality(&mut out, CompressionType::Best, FilterType::Adaptive);
    apply_metadata(&mut encoder, icc, exif)?;
    encoder
        .write_image(
            img.as_bytes(),
            img.width(),
            img.height(),
            img.color().into(),
        )
        .map_err(|e| e.to_string())?;
    Ok(out)
}

fn apply_metadata(
    encoder: &mut impl ImageEncoder,
    icc: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
) -> Result<(), String> {
    if let Some(icc) = icc {
        encoder
            .set_icc_profile(icc)
            .map_err(|_| "its colour profile can't be kept in the new format".to_string())?;
    }
    if let Some(exif) = exif {
        encoder
            .set_exif_metadata(exif)
            .map_err(|_| "its EXIF data can't be kept in the new format".to_string())?;
    }
    Ok(())
}

/// Pixel-exact comparison. 8-bit images are compared as RGBA since lossless
/// WebP may hand grayscale back as RGB; deeper images must match exactly.
fn same_pixels(a: &DynamicImage, b: &DynamicImage) -> bool {
    if a.width() != b.width() || a.height() != b.height() {
        return false;
    }
    if a.color() == b.color() {
        return a.as_bytes() == b.as_bytes();
    }
    let eight_bit = |c: ColorType| c.bytes_per_pixel() == c.channel_count();
    eight_bit(a.color()) && eight_bit(b.color()) && a.to_rgba8() == b.to_rgba8()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb, Rgba};

    /// A BMP with plenty of flat colour: uncompressed, so re-encoding shrinks it.
    fn sample_bmp(dir: &Path) -> std::path::PathBuf {
        let img = ImageBuffer::from_fn(64, 48, |x, y| Rgb([(x * 4) as u8, (y * 5) as u8, 80]));
        let path = dir.join("sample.bmp");
        img.save(&path).unwrap();
        path
    }

    #[test]
    fn bmp_to_lossless_webp_is_smaller_and_pixel_identical() {
        let dir = tempfile::tempdir().unwrap();
        let path = sample_bmp(dir.path());
        let (bytes, ext) = reencode_image(&path, ImageTarget::LosslessWebp).unwrap();
        assert_eq!(ext, "webp");
        assert!((bytes.len() as u64) < std::fs::metadata(&path).unwrap().len());
        let back = image::load_from_memory(&bytes).unwrap().to_rgb8();
        assert_eq!(back, image::open(&path).unwrap().to_rgb8());
    }

    #[test]
    fn sixteen_bit_images_stay_png_even_when_webp_is_requested() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep.png");
        let img: ImageBuffer<Rgba<u16>, Vec<u16>> = ImageBuffer::from_fn(8, 8, |x, y| {
            Rgba([x as u16 * 1000, y as u16 * 999, 7, 65535])
        });
        img.save(&path).unwrap();
        let (bytes, ext) = reencode_image(&path, ImageTarget::LosslessWebp).unwrap();
        assert_eq!(ext, "png");
        assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgba16(), img);
    }

    #[test]
    fn recognises_only_lossless_formats() {
        assert!(is_reencodable_image(Path::new("a.PNG")));
        assert!(is_reencodable_image(Path::new("scan.tiff")));
        assert!(!is_reencodable_image(Path::new("photo.jpg")));
        assert!(!is_reencodable_image(Path::new("clip.webp")));
    }
}
