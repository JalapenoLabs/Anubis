//! Bounded image decoding, and the seam an application screens images through.
//!
//! Every image a stranger uploads is hostile input in two ways. Its bytes may
//! be crafted to make a decoder allocate far more than they weigh, and its
//! pixels may be something the deployment must never store. This module
//! answers both, once, for every path that takes an image:
//!
//! - [`decode`] reads an upload under [`DecodeLimits`]. The format is read off
//!   the bytes, never a file name or a content type, and the dimensions are
//!   read from the header and held to the pixel budget **before** the pixel
//!   buffer is allocated, so a few hundred bytes declaring a gigapixel image
//!   cost a header parse rather than the instance's memory.
//! - [`ImageScreen`] is what an application implements to look at the decoded
//!   pixels before anything is stored, typically an explicit-content
//!   classifier. It answers a [`Verdict`] or a [`ScreenError`], and a caller
//!   refuses the upload on either a [`Verdict::Refuse`] or an error: a screen
//!   that could not look has not said yes.
//!
//! The avatar upload at `POST /auth/profile/avatar` runs both: decode, screen
//! when [`crate::auth::router_with`] was given one, then crop, encode and
//! store. An application screening its own uploads calls the same two pieces,
//! so one classifier covers every surface. The contract a screen implements is
//! written out in the repository's `docs/api.md`.
//!
//! [`IMAGE_REFUSED`] and [`IMAGE_SCREEN_UNAVAILABLE`] are the two error codes a
//! refused upload answers with. They are the framework's spelling, so an
//! application refusing its own uploads answers with the same ones and a
//! client handles every surface the same way.

mod screen;

use std::fmt;
use std::io::Cursor;

use image::{DynamicImage, ImageDecoder, ImageError, ImageReader, Limits};

#[doc(inline)]
pub use screen::{ImageScreen, ScreenError, ScreenFuture, Verdict};

/// The error code an upload refused by an [`ImageScreen`] answers with.
///
/// The response is a `400` whose message says only that the image cannot be
/// used. It never says which check refused it or why, because a refusal that
/// explained itself would teach somebody how to get past it.
pub const IMAGE_REFUSED: &str = "image_refused";

/// The error code an upload answers with when its [`ImageScreen`] failed.
///
/// The response is a `503`: nothing was stored, the screen's failure is logged
/// on the server, and the person may try again. A screen that could not decide
/// is a refusal, never a pass.
pub const IMAGE_SCREEN_UNAVAILABLE: &str = "image_screen_unavailable";

/// The longest edge [`DecodeLimits::default`] admits, in pixels.
///
/// Wide enough for a panorama, which the pixel budget then keeps honest: an
/// edge limit alone would admit 8,192 by 8,192, which is 64 megapixels.
pub const DEFAULT_MAX_EDGE: u32 = 8192;

/// The pixel count [`DecodeLimits::default`] admits: 4,096 squared.
///
/// About 16.8 megapixels, which covers a phone camera's default output and
/// caps one decode near 64 MiB at four bytes a pixel. Raising it raises what a
/// handful of concurrent uploads may hold at once, so derive it from the
/// instance's memory rather than from the largest photo anybody might send.
pub const DEFAULT_MAX_PIXELS: u64 = 16_777_216;

/// The widest pixel `image` can represent: 32-bit float RGBA, sixteen bytes.
///
/// The allocation ceiling handed to the decoder is the pixel budget at this
/// width, so it never refuses an image the pixel budget admitted and still
/// bounds a decoder whose header lied about what it would allocate.
const WIDEST_PIXEL_BYTES: u64 = 16;

/// How large an image [`decode`] will touch.
///
/// Both bounds are checked against the header, before the pixel buffer is
/// allocated. The avatar upload reads its limits from configuration; see
/// [`crate::config::AvatarConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
    /// The longest width or height admitted, in pixels.
    pub max_edge: u32,
    /// The most pixels admitted, width times height.
    pub max_pixels: u64,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_edge: DEFAULT_MAX_EDGE,
            max_pixels: DEFAULT_MAX_PIXELS,
        }
    }
}

/// The formats [`decode`] reads, detected from the bytes themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageFormat {
    Jpeg,
    Png,
    WebP,
    Gif,
}

impl ImageFormat {
    /// The format's media type, such as `image/png`.
    #[must_use]
    pub fn media_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::WebP => "image/webp",
            Self::Gif => "image/gif",
        }
    }
}

/// An image [`decode`] read and bounded, ready to screen or to process.
///
/// It holds the source as it arrived, uncropped and at full resolution (the
/// first frame of an animation), so a screen sees everything the upload
/// carried rather than only the part a later crop would keep. The pixels are
/// read through [`DecodedImage::to_rgba8`], which converts on demand: nothing
/// is copied for a caller that never asks.
pub struct DecodedImage {
    image: DynamicImage,
    format: ImageFormat,
}

impl DecodedImage {
    /// The width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.image.width()
    }

    /// The height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.image.height()
    }

    /// The format the bytes were in.
    #[must_use]
    pub fn format(&self) -> ImageFormat {
        self.format
    }

    /// The pixels as 8-bit RGBA, row-major from the top-left corner.
    ///
    /// Always `width * height * 4` bytes. Transparency is kept rather than
    /// flattened, so a screen sees what the uploader sent; an opaque source
    /// reads alpha `255` throughout. Each call converts afresh, so call it
    /// once and keep the buffer.
    #[must_use]
    pub fn to_rgba8(&self) -> Vec<u8> {
        self.image.to_rgba8().into_raw()
    }

    /// The decoded source, for the framework's own processing.
    pub(crate) fn source(&self) -> &DynamicImage {
        &self.image
    }
}

/// Names the image's shape and never its pixels, so a log line or a panic
/// message that formats one cannot carry the picture with it.
impl fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecodedImage")
            .field("width", &self.width())
            .field("height", &self.height())
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

/// Why [`decode`] refused an upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError {
    kind: DecodeErrorKind,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodeErrorKind {
    /// Not an image in a format this module reads, or a damaged one.
    Unreadable,
    /// A readable image larger than the limits admit.
    OverBudget,
}

impl DecodeError {
    /// Whether the image was refused for its size rather than its bytes.
    ///
    /// The two deserve different sentences: "upload a smaller picture" helps
    /// a person whose camera is large, and "that is not an image" helps
    /// nobody who sent one.
    #[must_use]
    pub fn is_over_budget(&self) -> bool {
        self.kind == DecodeErrorKind::OverBudget
    }

    fn unreadable(detail: impl fmt::Display) -> Self {
        Self {
            kind: DecodeErrorKind::Unreadable,
            detail: detail.to_string(),
        }
    }

    fn over_budget(detail: impl fmt::Display) -> Self {
        Self {
            kind: DecodeErrorKind::OverBudget,
            detail: detail.to_string(),
        }
    }

    /// Sorts the decoder's own error: a limit it enforced is a size refusal,
    /// and anything else is bytes it could not read.
    fn from_image(error: &ImageError) -> Self {
        if matches!(error, ImageError::Limits(_)) {
            return Self::over_budget(error);
        }
        Self::unreadable(error)
    }
}

/// Renders the decoder's detail, for a log line. Never shown to a client.
impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for DecodeError {}

/// Decodes `raw` as a JPEG, PNG, WebP or GIF image within `limits`.
///
/// The format is detected from the bytes. The header is read first and its
/// dimensions are held to both limits before the decoder allocates the pixel
/// buffer, so an image refused for its size costs a header parse. This is
/// synchronous, CPU-bound work: call it from `tokio::task::spawn_blocking`.
///
/// # Errors
/// A [`DecodeError`] when the bytes are not an image in one of the four
/// formats, are damaged, or declare an image larger than `limits` admit;
/// [`DecodeError::is_over_budget`] tells the last case apart.
pub fn decode(raw: &[u8], limits: &DecodeLimits) -> Result<DecodedImage, DecodeError> {
    let mut reader = ImageReader::new(Cursor::new(raw))
        .with_guessed_format()
        .map_err(DecodeError::unreadable)?;

    let format = match reader.format() {
        Some(image::ImageFormat::Jpeg) => ImageFormat::Jpeg,
        Some(image::ImageFormat::Png) => ImageFormat::Png,
        Some(image::ImageFormat::WebP) => ImageFormat::WebP,
        Some(image::ImageFormat::Gif) => ImageFormat::Gif,
        other => {
            return Err(DecodeError::unreadable(format_args!(
                "expected a JPEG, PNG, WebP or GIF image, found {other:?}"
            )));
        }
    };

    // The edge limits are enforced by the decoder as it reads the header. The
    // allocation ceiling is a second line behind the pixel check below, for a
    // decoder that allocates something its header did not declare.
    let mut decoder_limits = Limits::default();
    decoder_limits.max_image_width = Some(limits.max_edge);
    decoder_limits.max_image_height = Some(limits.max_edge);
    decoder_limits.max_alloc = Some(limits.max_pixels.saturating_mul(WIDEST_PIXEL_BYTES));
    reader.limits(decoder_limits);

    let decoder = reader
        .into_decoder()
        .map_err(|error| DecodeError::from_image(&error))?;

    let (width, height) = decoder.dimensions();
    let pixels = u64::from(width) * u64::from(height);
    if pixels > limits.max_pixels {
        return Err(DecodeError::over_budget(format_args!(
            "{width}x{height} is {pixels} pixels, over the budget of {}",
            limits.max_pixels
        )));
    }

    let image =
        DynamicImage::from_decoder(decoder).map_err(|error| DecodeError::from_image(&error))?;
    Ok(DecodedImage { image, format })
}

#[cfg(test)]
mod tests {
    use image::codecs::png::PngEncoder;

    use super::{DecodeLimits, DecodedImage, ImageFormat, decode};

    fn png_bytes(image: &image::DynamicImage) -> Vec<u8> {
        let mut bytes = Vec::new();
        image
            .write_with_encoder(PngEncoder::new(&mut bytes))
            .expect("encoding the fixture must succeed");
        bytes
    }

    /// A GIF that is all header: a logical screen of `width` by `height` and no
    /// frame data.
    ///
    /// GIF carries its dimensions in plain little-endian with no checksum, so a
    /// test can declare any size in a few bytes. Decoding it for real would
    /// need a buffer of `width * height` pixels; refusing it must not.
    fn gif_header_declaring(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        // No global color table, background color 0, square pixels.
        bytes.extend_from_slice(&[0x00, 0x00, 0x00]);
        // An image descriptor covering the whole screen, then the trailer.
        bytes.push(0x2C);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&[0x00, 0x02, 0x00, 0x3B]);
        bytes
    }

    #[test]
    fn a_png_decodes_with_its_shape_and_format() {
        let source = image::DynamicImage::new_rgb8(120, 80);
        let decoded =
            decode(&png_bytes(&source), &DecodeLimits::default()).expect("a small PNG decodes");

        assert_eq!(decoded.width(), 120);
        assert_eq!(decoded.height(), 80);
        assert_eq!(decoded.format(), ImageFormat::Png);
        assert_eq!(decoded.format().media_type(), "image/png");
    }

    #[test]
    fn rgba_is_four_bytes_a_pixel_and_keeps_transparency() {
        let mut source = image::RgbaImage::new(3, 2);
        source.put_pixel(0, 0, image::Rgba([10, 20, 30, 0]));
        let decoded = decode(
            &png_bytes(&image::DynamicImage::ImageRgba8(source)),
            &DecodeLimits::default(),
        )
        .expect("a small PNG decodes");

        let rgba = decoded.to_rgba8();
        assert_eq!(rgba.len(), 3 * 2 * 4);
        assert_eq!(&rgba[0..4], &[10, 20, 30, 0], "alpha reaches the screen");
    }

    #[test]
    fn an_opaque_source_reads_fully_opaque() {
        let source = image::DynamicImage::new_rgb8(4, 4);
        let decoded =
            decode(&png_bytes(&source), &DecodeLimits::default()).expect("a small PNG decodes");

        let rgba = decoded.to_rgba8();
        assert!(rgba.chunks_exact(4).all(|pixel| pixel[3] == 255));
    }

    #[test]
    fn junk_is_unreadable_rather_than_over_budget() {
        let error = decode(b"definitely not an image", &DecodeLimits::default())
            .expect_err("junk must be refused");

        assert!(!error.is_over_budget());
    }

    #[test]
    fn an_image_past_the_pixel_budget_is_refused_from_its_header() {
        // 8,000 a side is inside the default edge limit and is 64 megapixels,
        // four times the default budget. The fixture is a few dozen bytes, so
        // reaching a refusal at all proves the buffer was never allocated:
        // decoding it would need 64 MB that the bytes cannot supply.
        let bytes = gif_header_declaring(8000, 8000);
        let error = decode(&bytes, &DecodeLimits::default())
            .expect_err("a header declaring 64 megapixels must be refused");

        assert!(error.is_over_budget(), "refused for its size: {error}");
    }

    #[test]
    fn an_edge_past_the_limit_is_refused_from_its_header() {
        let bytes = gif_header_declaring(u16::MAX, 2);
        let error = decode(&bytes, &DecodeLimits::default())
            .expect_err("an edge of 65,535 must be refused");

        assert!(error.is_over_budget(), "refused for its size: {error}");
    }

    #[test]
    fn the_limits_are_the_callers() {
        let source = image::DynamicImage::new_rgb8(100, 100);
        let tight = DecodeLimits {
            max_edge: 1000,
            max_pixels: 9_999,
        };

        let error = decode(&png_bytes(&source), &tight).expect_err("10,000 pixels exceed 9,999");
        assert!(error.is_over_budget());
    }

    #[test]
    fn debug_names_the_shape_and_never_the_pixels() {
        let source = image::DynamicImage::new_rgb8(5, 7);
        let decoded: DecodedImage =
            decode(&png_bytes(&source), &DecodeLimits::default()).expect("a small PNG decodes");

        let rendered = format!("{decoded:?}");
        assert!(rendered.contains("width: 5"), "{rendered}");
        assert!(rendered.contains("height: 7"), "{rendered}");
        assert!(rendered.contains("Png"), "{rendered}");
        assert!(rendered.len() < 100, "no buffer in the output: {rendered}");
    }
}
