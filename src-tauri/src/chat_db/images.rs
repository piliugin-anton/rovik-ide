//! Images attached in the chat input, made ready for models: whatever type
//! they come in (PNG, JPEG, WebP, GIF), they're stored as PNGs at most
//! `MAX_SIDE` pixels a side, the right way up.
//!
//! - Read by `image`, within limits: a damaged or hostile file can't claim
//!   more than `MAX_READ_SIDE` pixels a side, nor take more than 512 MB.
//! - Scaled by `fast_image_resize` with Lanczos3 convolution: thin lines and
//!   small text keep their share of each pixel, as in Pillow (its faster
//!   modes skip pixels and drop them). It premultiplies transparency, which
//!   `image`'s own resize doesn't: hidden colors would bleed into the edges.
//! - Turned by their EXIF orientation, as photo viewers show them.
//! - An animation keeps its first frame.
//! - Color profiles aren't kept: the PNG holds the pixels as decoded (a
//!   CMYK JPEG's profile wouldn't even fit an RGB PNG).

use fast_image_resize as fir;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::error::LimitErrorKind;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageError, ImageReader, Limits};
use std::io::Cursor;

/// Longest side an image is stored and sent with.
pub const MAX_SIDE: u32 = 768;
/// What attached images are stored as.
pub const MIME: &str = "image/png";
/// Longest side read: a bigger image is refused before it's decoded.
const MAX_READ_SIDE: u32 = 16_384;

/// An attached image, ready to store.
#[derive(Debug)]
pub struct Prepared {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Reads an image, fits it into `MAX_SIDE`×`MAX_SIDE` and encodes it as a
/// PNG. CPU-bound: a big photo takes a few hundred milliseconds.
pub fn prepare(bytes: &[u8]) -> Result<Prepared, String> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| UNREADABLE.to_string())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_READ_SIDE);
    limits.max_image_height = Some(MAX_READ_SIDE);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(read_error)?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let image = DynamicImage::from_decoder(decoder).map_err(read_error)?;

    let (width, height) = fitted(image.width(), image.height());
    let mut image = if (width, height) == (image.width(), image.height()) {
        image
    } else {
        scaled(&image, width, height)?
    };
    // Turned once small, where it's cheap: a quarter turn swaps the sides,
    // and either way round they fit the square.
    image.apply_orientation(orientation);
    let image = eight_bit(image);

    let mut png = Vec::new();
    PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::Adaptive)
        .write_image(
            image.as_bytes(),
            image.width(),
            image.height(),
            image.color().into(),
        )
        .map_err(|e| format!("The image couldn't be converted: {e}"))?;
    Ok(Prepared {
        png,
        width: image.width(),
        height: image.height(),
    })
}

const UNREADABLE: &str = "The image couldn't be read: the file may be damaged.";

fn read_error(e: ImageError) -> String {
    match e {
        ImageError::Limits(e) if matches!(e.kind(), LimitErrorKind::DimensionError) => {
            format!("The image is too large: it can be {MAX_READ_SIDE} pixels a side at most.")
        }
        ImageError::Limits(_) => "The image is too large to read.".into(),
        _ => UNREADABLE.into(),
    }
}

/// The size an image is sent at: within `MAX_SIDE`×`MAX_SIDE`, its shape
/// kept, never enlarged.
fn fitted(width: u32, height: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= MAX_SIDE {
        return (width, height);
    }
    let scale = f64::from(MAX_SIDE) / f64::from(longest);
    let side = |n: u32| ((f64::from(n) * scale).round() as u32).clamp(1, MAX_SIDE);
    (side(width), side(height))
}

fn scaled(image: &DynamicImage, width: u32, height: u32) -> Result<DynamicImage, String> {
    let mut out = DynamicImage::new(width, height, image.color());
    fir::Resizer::new()
        .resize(
            image,
            &mut out,
            &fir::ResizeOptions::new()
                .resize_alg(fir::ResizeAlg::Convolution(fir::FilterType::Lanczos3)),
        )
        .map_err(|e| format!("The image couldn't be scaled down: {e}"))?;
    Ok(out)
}

/// 8 bits a channel, and no alpha channel when nothing is see-through
/// (images pasted from the clipboard come with one whatever they show).
fn eight_bit(image: DynamicImage) -> DynamicImage {
    let color = image.color();
    let alpha = color.has_alpha() && !opaque(&image);
    match (color.has_color(), alpha) {
        (true, true) => image.into_rgba8().into(),
        (true, false) => image.into_rgb8().into(),
        (false, true) => image.into_luma_alpha8().into(),
        (false, false) => image.into_luma8().into(),
    }
}

fn opaque(image: &DynamicImage) -> bool {
    match image {
        DynamicImage::ImageLumaA8(i) => i.pixels().all(|p| p[1] == u8::MAX),
        DynamicImage::ImageRgba8(i) => i.pixels().all(|p| p[3] == u8::MAX),
        DynamicImage::ImageLumaA16(i) => i.pixels().all(|p| p[1] == u16::MAX),
        DynamicImage::ImageRgba16(i) => i.pixels().all(|p| p[3] == u16::MAX),
        DynamicImage::ImageRgba32F(i) => i.pixels().all(|p| p[3] >= 1.0),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::gif::GifEncoder;
    use image::codecs::jpeg::JpegEncoder;
    use image::{ColorType, Frame, GenericImageView, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};

    fn encoded(image: &DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    fn decoded(prepared: &Prepared) -> DynamicImage {
        assert!(prepared.png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let image = image::load_from_memory_with_format(&prepared.png, ImageFormat::Png).unwrap();
        assert_eq!(
            (image.width(), image.height()),
            (prepared.width, prepared.height)
        );
        image
    }

    /// Left half red, right half blue.
    fn halves(width: u32, height: u32) -> RgbImage {
        RgbImage::from_fn(width, height, |x, _| {
            if x < width / 2 {
                Rgb([230, 20, 20])
            } else {
                Rgb([20, 20, 230])
            }
        })
    }

    #[test]
    fn a_big_image_is_fitted_into_the_square_and_a_small_one_kept() {
        let big = DynamicImage::from(halves(1600, 900));
        let prepared = prepare(&encoded(&big, ImageFormat::Png)).unwrap();
        assert_eq!((prepared.width, prepared.height), (768, 432));
        let tall = DynamicImage::from(halves(500, 4000));
        let prepared = prepare(&encoded(&tall, ImageFormat::Png)).unwrap();
        assert_eq!((prepared.width, prepared.height), (96, 768));
        // Never enlarged.
        let small = DynamicImage::from(halves(300, 200));
        let prepared = prepare(&encoded(&small, ImageFormat::Png)).unwrap();
        assert_eq!((prepared.width, prepared.height), (300, 200));
        assert_eq!(decoded(&prepared).to_rgb8(), halves(300, 200));
        // A sliver keeps a pixel.
        assert_eq!(fitted(100_000, 10), (768, 1));
    }

    #[test]
    fn every_type_models_take_becomes_a_png() {
        let image = DynamicImage::from(halves(1000, 500));
        for format in [
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::WebP,
            ImageFormat::Gif,
        ] {
            let prepared = prepare(&encoded(&image, format)).unwrap();
            let out = decoded(&prepared).to_rgb8();
            assert_eq!(out.dimensions(), (768, 384), "{format:?}");
            let (red, blue) = (out.get_pixel(100, 200), out.get_pixel(668, 200));
            assert!(red[0] > 200 && red[2] < 60, "{format:?}: {red:?}");
            assert!(blue[2] > 200 && blue[0] < 60, "{format:?}: {blue:?}");
        }
    }

    #[test]
    fn a_photo_is_turned_by_its_exif_orientation() {
        // EXIF (big-endian TIFF): one entry, Orientation (0x0112) = 6, a
        // quarter turn clockwise to show it.
        let exif = [
            b"MM\0\x2a\0\0\0\x08\0\x01".as_slice(),
            &[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0],
            &[0, 0, 0, 0],
        ]
        .concat();
        let mut jpeg = Vec::new();
        let mut encoder = JpegEncoder::new_with_quality(&mut jpeg, 90);
        encoder.set_exif_metadata(exif).unwrap();
        let image = halves(1600, 800);
        encoder
            .write_image(image.as_raw(), 1600, 800, ColorType::Rgb8.into())
            .unwrap();
        let prepared = prepare(&jpeg).unwrap();
        assert_eq!((prepared.width, prepared.height), (384, 768));
        let out = decoded(&prepared).to_rgb8();
        // The left side, turned clockwise, is on top.
        let (top, bottom) = (out.get_pixel(192, 100), out.get_pixel(192, 668));
        assert!(top[0] > 200 && top[2] < 60, "{top:?}");
        assert!(bottom[2] > 200 && bottom[0] < 60, "{bottom:?}");
    }

    #[test]
    fn transparency_is_kept_without_dark_edges_and_an_opaque_alpha_dropped() {
        // White on the left, see-through (with black behind it) on the right:
        // scaled, the edge fades out, it doesn't darken.
        let image = RgbaImage::from_fn(2000, 1000, |x, _| {
            if x < 1001 {
                Rgba([255, 255, 255, 255])
            } else {
                Rgba([0, 0, 0, 0])
            }
        });
        let prepared = prepare(&encoded(&image.into(), ImageFormat::Png)).unwrap();
        let out = decoded(&prepared);
        assert_eq!(out.color(), ColorType::Rgba8);
        let out = out.to_rgba8();
        let edge: Vec<_> = out.pixels().filter(|p| p[3] > 0 && p[3] < 255).collect();
        assert!(!edge.is_empty());
        assert!(edge.iter().all(|p| p[0] >= 250), "{edge:?}");

        // Clipboard images come as RGBA, see-through or not.
        let opaque = DynamicImage::from(DynamicImage::from(halves(400, 300)).to_rgba8());
        let out = decoded(&prepare(&encoded(&opaque, ImageFormat::Png)).unwrap());
        assert_eq!(out.color(), ColorType::Rgb8);
    }

    #[test]
    fn an_animation_keeps_its_first_frame() {
        let frame = |color: [u8; 4]| Frame::new(RgbaImage::from_pixel(40, 30, Rgba(color)));
        let mut gif = Vec::new();
        GifEncoder::new(&mut gif)
            .encode_frames([frame([255, 0, 0, 255]), frame([0, 0, 255, 255])])
            .unwrap();
        let out = decoded(&prepare(&gif).unwrap());
        assert_eq!(out.dimensions(), (40, 30));
        let pixel = out.to_rgb8().get_pixel(20, 15).0;
        assert!(pixel[0] > 240 && pixel[2] < 15, "{pixel:?}");
    }

    #[test]
    fn deep_and_gray_images_become_eight_bit_and_stay_gray() {
        let deep = DynamicImage::from(halves(64, 64)).to_rgb16();
        let out = decoded(&prepare(&encoded(&deep.into(), ImageFormat::Png)).unwrap());
        assert_eq!(out.color(), ColorType::Rgb8);
        let gray = DynamicImage::from(halves(2000, 100)).to_luma8();
        let out = decoded(&prepare(&encoded(&gray.into(), ImageFormat::Png)).unwrap());
        assert_eq!(out.color(), ColorType::L8);
        assert_eq!(out.dimensions(), (768, 38));
    }

    #[test]
    fn what_cant_be_read_is_refused() {
        let wide = DynamicImage::from(RgbImage::new(MAX_READ_SIDE + 1, 1));
        let err = prepare(&encoded(&wide, ImageFormat::Png)).unwrap_err();
        assert!(err.contains("16384 pixels a side"), "{err}");
        let err = prepare(b"\x89PNG\r\n\x1a\nnot much of an image").unwrap_err();
        assert_eq!(err, UNREADABLE);
        assert_eq!(prepare(b"").unwrap_err(), UNREADABLE);
    }
}
