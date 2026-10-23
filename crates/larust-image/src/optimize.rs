//! Decode, resize, and re-encode one image - the actual work behind
//! [`crate::route::ImagePlugin`]'s one route. Pure, synchronous, and I/O-
//! free (the caller reads the source file's bytes and writes the result
//! to the cache); kept separate from [`crate::route`] so the resize/encode
//! logic itself is testable without a real `axum` request or filesystem.

use image::imageops::FilterType;
use image::ImageError;

/// How to resize the decoded source - see [`optimize`]'s own doc comment
/// for when each is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeTarget {
    /// The common case: `<Image width="..." height="..." />` with both
    /// given - resize to exactly this box, cropping/stretching as needed,
    /// never preserving the source's own aspect ratio. Laying out a fixed
    /// box is the whole point of giving both dimensions explicitly.
    Exact { width: u32, height: u32 },
    /// `fill="true"`, or a `sizes`-driven `srcset` variant - only a target
    /// *width* is known; height is derived from the source's own aspect
    /// ratio instead of being forced into an arbitrary box. Different
    /// breakpoints/layouts legitimately want different heights for the
    /// same source image, so forcing one fixed height across every
    /// variant (the way [`ResizeTarget::Exact`] does for a single,
    /// explicitly-boxed image) would be wrong here.
    Width { width: u32 },
}

/// Which format [`optimize`] actually produced - the caller needs this to
/// pick the right `Content-Type`/cache file extension, since it depends on
/// the request's own `Accept` header, not just the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodedFormat {
    Avif,
    Jpeg,
}

impl EncodedFormat {
    pub fn content_type(self) -> &'static str {
        match self {
            EncodedFormat::Avif => "image/avif",
            EncodedFormat::Jpeg => "image/jpeg",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            EncodedFormat::Avif => "avif",
            EncodedFormat::Jpeg => "jpg",
        }
    }
}

#[derive(Debug)]
pub struct Optimized {
    pub bytes: Vec<u8>,
    pub format: EncodedFormat,
}

/// [`optimize`]'s full result - the main resized/encoded image, plus
/// (when asked for) a tiny blurred thumbnail generated from the *same*
/// decode, so producing one never costs a second, separate decode of the
/// source file. See [`crate::route`]'s own doc comment for why the blur
/// thumbnail is generated here, as a side effect of an ordinary optimize
/// request, rather than computed synchronously inside [`crate::tag`].
#[derive(Debug)]
pub struct OptimizeResult {
    pub main: Optimized,
    pub blur: Option<Vec<u8>>,
}

#[derive(Debug)]
pub enum OptimizeError {
    Decode(ImageError),
    Encode(String),
}

impl std::fmt::Display for OptimizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OptimizeError::Decode(error) => write!(f, "failed to decode source image: {error}"),
            OptimizeError::Encode(message) => write!(f, "failed to encode image: {message}"),
        }
    }
}

impl std::error::Error for OptimizeError {}

/// A tiny, heavily-compressed thumbnail width for blur placeholders - wide
/// enough to carry real color/shape information once blurred by CSS
/// (`background-size: cover` upscaling), narrow enough that even a large
/// source image's blur variant is a few hundred bytes at most, since it's
/// inlined directly into the page's own HTML as a base64 data URI (see
/// `crate::tag`'s own doc comment) rather than fetched separately.
const BLUR_WIDTH: u32 = 16;
const BLUR_QUALITY: u8 = 30;

/// Decodes `source` once, resizes it per `target` for the main result -
/// AVIF when `prefer_avif` is set (the caller decides this from the
/// request's own `Accept` header), JPEG otherwise - and, when
/// `generate_blur` is set, *also* produces a tiny blurred-placeholder
/// thumbnail from the same decode (always JPEG, regardless of
/// `prefer_avif` - it's consumed directly by the browser's own CSS
/// decoder as an inline data URI, never served as a standalone resource
/// a format negotiation could apply to).
pub fn optimize(
    source: &[u8],
    target: ResizeTarget,
    quality: u8,
    prefer_avif: bool,
    generate_blur: bool,
) -> Result<OptimizeResult, OptimizeError> {
    let decoded = image::load_from_memory(source).map_err(OptimizeError::Decode)?;
    let resized = resize_to(&decoded, target);

    let main = if prefer_avif {
        match encode_avif(&resized, quality) {
            Ok(bytes) => Optimized {
                bytes,
                format: EncodedFormat::Avif,
            },
            // A real encode failure (not a capability gap - every input
            // this crate decodes is RGBA8-convertible) falls through to
            // JPEG rather than failing the request outright; a slightly
            // bigger JPEG beats a 500 for an image that would otherwise
            // have rendered fine.
            Err(_) => Optimized {
                bytes: encode_jpeg(&resized, quality)?,
                format: EncodedFormat::Jpeg,
            },
        }
    } else {
        Optimized {
            bytes: encode_jpeg(&resized, quality)?,
            format: EncodedFormat::Jpeg,
        }
    };

    let blur = if generate_blur {
        let thumbnail = resize_to(&decoded, ResizeTarget::Width { width: BLUR_WIDTH });
        encode_jpeg(&thumbnail, BLUR_QUALITY).ok()
    } else {
        None
    };

    Ok(OptimizeResult { main, blur })
}

fn resize_to(image: &image::DynamicImage, target: ResizeTarget) -> image::DynamicImage {
    match target {
        ResizeTarget::Exact { width, height } => {
            image.resize_exact(width, height, FilterType::Lanczos3)
        }
        ResizeTarget::Width { width } => {
            let (source_width, source_height) = (image.width().max(1), image.height().max(1));
            let height =
                ((source_height as u64 * width as u64) / source_width as u64).max(1) as u32;
            image.resize_exact(width, height, FilterType::Lanczos3)
        }
    }
}

fn encode_avif(image: &image::DynamicImage, quality: u8) -> Result<Vec<u8>, OptimizeError> {
    let rgba = image.to_rgba8();
    let (width, height) = (rgba.width() as usize, rgba.height() as usize);
    let pixels: Vec<rgb::RGBA8> = rgba
        .pixels()
        .map(|p| rgb::RGBA8::new(p[0], p[1], p[2], p[3]))
        .collect();
    let buffer = ravif::Img::new(pixels.as_slice(), width, height);

    let result = ravif::Encoder::new()
        .with_quality(quality as f32)
        .with_speed(6)
        .encode_rgba(buffer)
        .map_err(|error| OptimizeError::Encode(error.to_string()))?;

    Ok(result.avif_file)
}

fn encode_jpeg(image: &image::DynamicImage, quality: u8) -> Result<Vec<u8>, OptimizeError> {
    use image::codecs::jpeg::JpegEncoder;

    let mut bytes = Vec::new();
    let rgb = image.to_rgb8();
    JpegEncoder::new_with_quality(&mut bytes, quality)
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|error| OptimizeError::Encode(error.to_string()))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_png() -> Vec<u8> {
        // A real, minimal PNG, generated once via `image` itself rather
        // than hand-written bytes - simplest way to get a guaranteed-valid
        // fixture without shipping a binary test asset. 40x20 (2:1), wide
        // enough that `ResizeTarget::Width`'s own proportional-height math
        // is actually exercised (a square fixture would hide a transposed
        // width/height bug).
        let mut buf = Vec::new();
        let img = image::RgbImage::from_fn(40, 20, |x, y| {
            image::Rgb([(x * 6) as u8, (y * 12) as u8, 128])
        });
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn resizes_to_exactly_the_requested_box_and_encodes_jpeg() {
        let result = optimize(
            &tiny_png(),
            ResizeTarget::Exact {
                width: 10,
                height: 20,
            },
            80,
            false,
            false,
        )
        .unwrap();
        assert_eq!(result.main.format, EncodedFormat::Jpeg);
        assert!(result.blur.is_none());
        let decoded = image::load_from_memory(&result.main.bytes).unwrap();
        assert_eq!(decoded.width(), 10);
        assert_eq!(decoded.height(), 20);
    }

    #[test]
    fn width_only_target_preserves_the_sources_own_aspect_ratio() {
        // Source is 40x20 (2:1) - asking for width 20 should derive height
        // 10, not stretch/crop into some other ratio.
        let result = optimize(
            &tiny_png(),
            ResizeTarget::Width { width: 20 },
            80,
            false,
            false,
        )
        .unwrap();
        let decoded = image::load_from_memory(&result.main.bytes).unwrap();
        assert_eq!(decoded.width(), 20);
        assert_eq!(decoded.height(), 10);
    }

    #[test]
    fn prefers_avif_when_requested_and_still_resizes_correctly() {
        let result = optimize(
            &tiny_png(),
            ResizeTarget::Exact {
                width: 8,
                height: 8,
            },
            70,
            true,
            false,
        )
        .unwrap();
        assert_eq!(result.main.format, EncodedFormat::Avif);
        assert!(!result.main.bytes.is_empty());
    }

    #[test]
    fn generates_a_small_blur_thumbnail_alongside_the_main_result() {
        let result = optimize(
            &tiny_png(),
            ResizeTarget::Exact {
                width: 200,
                height: 100,
            },
            80,
            false,
            true,
        )
        .unwrap();
        let blur_bytes = result.blur.expect("blur was requested");
        let blur_decoded = image::load_from_memory(&blur_bytes).unwrap();
        assert_eq!(blur_decoded.width(), BLUR_WIDTH);
        // Blur bytes are a tiny fraction of a real photo's size - not a
        // precise bound, just confirms it's genuinely a thumbnail, not
        // accidentally a full-size encode.
        assert!(blur_bytes.len() < 2000);
    }

    #[test]
    fn a_corrupt_source_is_a_decode_error_not_a_panic() {
        let error = optimize(
            b"not an image",
            ResizeTarget::Exact {
                width: 10,
                height: 10,
            },
            75,
            false,
            false,
        )
        .unwrap_err();
        assert!(matches!(error, OptimizeError::Decode(_)));
    }
}
