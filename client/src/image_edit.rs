use std::io::Cursor;

use base64::Engine as _;
use image::{AnimationDecoder, ImageDecoder, ImageReader, RgbaImage};

const MAX_INPUT_BYTES: usize = 15_000_000;
const MAX_PIXELS: u64 = 16_000_000;
const MAX_GIF_BYTES: usize = 2_000_000;

pub struct DecodedImage {
    pub pixels: RgbaImage,
    pub preview: String,
    frames: Vec<image::Frame>,
    original_bytes: usize,
    repeat: image::codecs::gif::Repeat,
}

impl DecodedImage {
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }
}

pub fn decode(src: &str) -> Result<DecodedImage, String> {
    let (header, payload) = src.split_once(',').ok_or("Invalid image data.")?;
    if !header.starts_with("data:image/") || !header.ends_with(";base64") {
        return Err("Expected an embedded image.".into());
    }
    if payload.len() > MAX_INPUT_BYTES.div_ceil(3) * 4 {
        return Err("That image is too large to edit.".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|_| "Invalid image data.")?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err("That image is too large to edit.".into());
    }
    let original_bytes = bytes.len();
    let animated = bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a");
    let mut frames = Vec::new();
    let mut repeat = image::codecs::gif::Repeat::Infinite;
    if animated {
        let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(&bytes))
            .map_err(|e| format!("Couldn't open that GIF: {e}"))?;
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(MAX_PIXELS * 8);
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        decoder.set_limits(limits).map_err(|e| e.to_string())?;
        repeat = match decoder.loop_count() {
            image::metadata::LoopCount::Infinite => image::codecs::gif::Repeat::Infinite,
            image::metadata::LoopCount::Finite(n) => {
                image::codecs::gif::Repeat::Finite(n.get().min(u32::from(u16::MAX)) as u16)
            }
        };
        let (width, height) = decoder.dimensions();
        let frame_pixels = u64::from(width) * u64::from(height);
        if frame_pixels == 0 || frame_pixels > MAX_PIXELS {
            return Err("That GIF has too many pixels.".into());
        }
        for frame in decoder.into_frames() {
            if frames.len() >= 180 || frame_pixels * (frames.len() as u64 + 1) > 32_000_000 {
                return Err(
                    "That animation is too large to edit (180 frames / 32 million pixels maximum)."
                        .into(),
                );
            }
            frames.push(frame.map_err(|e| format!("Couldn't decode that GIF: {e}"))?);
        }
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("Couldn't identify that image: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(MAX_PIXELS * 8);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("Couldn't open that image: {e}"))?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err("That image has too many pixels to edit.".into());
    }
    let orientation = decoder
        .orientation()
        .map_err(|e| format!("Couldn't read image orientation: {e}"))?;
    let mut image = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("Couldn't decode that image: {e}"))?;
    image.apply_orientation(orientation);
    let pixels = image.into_rgba8();
    let preview = if pixels.width() > 1024 || pixels.height() > 1024 {
        let edge = u64::from(pixels.width().max(pixels.height()));
        let width = (u64::from(pixels.width()) * 1024 / edge).max(1) as u32;
        let height = (u64::from(pixels.height()) * 1024 / edge).max(1) as u32;
        png_url(&image::imageops::thumbnail(&pixels, width, height))?
    } else {
        png_url(&pixels)?
    };
    let preview = if frames.len() > 1 {
        src.to_owned()
    } else {
        preview
    };
    Ok(DecodedImage {
        pixels,
        preview,
        frames,
        original_bytes,
        repeat,
    })
}

pub fn crop_image(
    image: &DecodedImage,
    rect: [f64; 4],
    output: (u32, u32),
    jpeg: bool,
) -> Result<String, String> {
    if !image.is_animated() {
        return crop(&image.pixels, rect, output, jpeg);
    }
    let (left, top, width, height) = crop_bounds(&image.pixels, rect, output)?;
    // Re-encoding composited frames loses the original GIF's delta compression.
    if left == 0
        && top == 0
        && (width, height) == image.pixels.dimensions()
        && width <= output.0
        && height <= output.1
        && (u64::from(width) * u64::from(output.1))
            .abs_diff(u64::from(height) * u64::from(output.0))
            <= u64::from(width.max(height))
        && image.original_bytes <= MAX_GIF_BYTES
    {
        return Ok(image.preview.clone());
    }
    let scale = (f64::from(width) / f64::from(output.0))
        .min(f64::from(height) / f64::from(output.1))
        .min(1.0);
    let mut output = (
        (f64::from(output.0) * scale).floor().max(1.0) as u32,
        (f64::from(output.1) * scale).floor().max(1.0) as u32,
    );
    loop {
        if let Some(bytes) = encode_gif(&image.frames, rect, output, image.repeat)? {
            return Ok(data_url("image/gif", &bytes));
        }
        if output.0.max(output.1) <= 64 {
            return Err("That animation cannot fit within 2 MB. Choose a shorter GIF.".into());
        }
        output = ((output.0 * 3 / 4).max(1), (output.1 * 3 / 4).max(1));
    }
}

fn encode_gif(
    frames: &[image::Frame],
    rect: [f64; 4],
    output: (u32, u32),
    repeat: image::codecs::gif::Repeat,
) -> Result<Option<Vec<u8>>, String> {
    let mut bytes = Vec::new();
    let mut writer = GifOutput {
        bytes: &mut bytes,
        exceeded: false,
    };
    let result = (|| {
        let mut encoder = image::codecs::gif::GifEncoder::new_with_speed(&mut writer, 10);
        encoder.set_repeat(repeat).map_err(|e| e.to_string())?;
        for frame in frames {
            let pixels = crop_pixels(frame.buffer(), rect, output)?;
            encoder
                .encode_frame(image::Frame::from_parts(pixels, 0, 0, frame.delay()))
                .map_err(|e| format!("Couldn't export that GIF: {e}"))?;
        }
        Ok::<(), String>(())
    })();
    if writer.exceeded {
        return Ok(None);
    }
    result?;
    Ok(Some(bytes))
}

struct GifOutput<'a> {
    bytes: &'a mut Vec<u8>,
    exceeded: bool,
}
impl std::io::Write for GifOutput<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(data.len()) > MAX_GIF_BYTES {
            self.exceeded = true;
            return Err(std::io::Error::other("The cropped GIF exceeds 2 MB."));
        }
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn crop(
    pixels: &RgbaImage,
    rect: [f64; 4],
    output: (u32, u32),
    jpeg: bool,
) -> Result<String, String> {
    let resized = crop_pixels(pixels, rect, output)?;
    if !jpeg {
        return png_url(&resized);
    }
    let rgb = image::RgbImage::from_fn(output.0, output.1, |x, y| {
        let rgba = resized.get_pixel(x, y).0;
        let alpha = u32::from(rgba[3]);
        image::Rgb(std::array::from_fn(|i| {
            ((u32::from(rgba[i]) * alpha + 255 * (255 - alpha) + 127) / 255) as u8
        }))
    });
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 92)
        .encode_image(&rgb)
        .map_err(|e| format!("Couldn't export that image: {e}"))?;
    Ok(data_url("image/jpeg", &bytes))
}

fn crop_pixels(
    pixels: &RgbaImage,
    rect: [f64; 4],
    output: (u32, u32),
) -> Result<RgbaImage, String> {
    let (left, top, width, height) = crop_bounds(pixels, rect, output)?;
    let region = image::imageops::crop_imm(pixels, left, top, width, height);
    let mut region = region.to_image();
    // Transparent RGB must not bleed into visible pixels while interpolating edges.
    for pixel in region.pixels_mut() {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
    let mut resized = image::imageops::resize(
        &region,
        output.0,
        output.1,
        image::imageops::FilterType::Lanczos3,
    );
    for pixel in resized.pixels_mut() {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel = (u32::from(*channel) * 255 + alpha / 2)
                .checked_div(alpha)
                .unwrap_or(0)
                .min(255) as u8;
        }
    }
    Ok(resized)
}

fn crop_bounds(
    pixels: &RgbaImage,
    rect: [f64; 4],
    output: (u32, u32),
) -> Result<(u32, u32, u32, u32), String> {
    let [x, y, width, height] = rect;
    if !rect.iter().all(|v| v.is_finite())
        || pixels.width() == 0
        || pixels.height() == 0
        || x < -0.001
        || y < -0.001
        || width <= 0.0
        || height <= 0.0
        || x + width > f64::from(pixels.width()) + 0.001
        || y + height > f64::from(pixels.height()) + 0.001
        || output.0 == 0
        || output.1 == 0
        || output.0 > 1024
        || output.1 > 1024
    {
        return Err("Invalid image crop.".into());
    }
    let left = (x.max(0.0).round() as u32).min(pixels.width() - 1);
    let top = (y.max(0.0).round() as u32).min(pixels.height() - 1);
    let width = (width.round() as u32).min(pixels.width() - left).max(1);
    let height = (height.round() as u32).min(pixels.height() - top).max(1);
    Ok((left, top, width, height))
}

fn png_url(pixels: &RgbaImage) -> Result<String, String> {
    let mut bytes = Cursor::new(Vec::new());
    pixels
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|e| format!("Couldn't export that image: {e}"))?;
    Ok(data_url("image/png", bytes.get_ref()))
}

fn data_url(mime: &str, bytes: &[u8]) -> String {
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTIMIZED_GIF: &[u8] = include_bytes!("../tests/fixtures/optimized-profile.gif");

    #[test]
    fn an_uncropped_small_gif_keeps_its_original_compression_and_loop_count() {
        assert!(OPTIMIZED_GIF.len() < 1_000_000);
        let source = data_url("image/gif", OPTIMIZED_GIF);
        let image = decode(&source).unwrap();
        assert_eq!(image.frames.len(), 28);
        assert_eq!(
            crop_image(&image, [0.0, 0.0, 384.0, 384.0], (512, 512), false).unwrap(),
            source
        );
        assert!(crop_image(&image, [f64::NAN, 0.0, 384.0, 384.0], (512, 512), false).is_err());
        assert!(crop_image(&image, [0.0, 0.0, 384.0, 384.0], (2048, 2048), false).is_err());
    }

    #[test]
    fn a_small_optimized_gif_fits_after_cropping_without_losing_frames_or_timing() {
        let image = decode(&data_url("image/gif", OPTIMIZED_GIF)).unwrap();
        let rect = [16.0, 16.0, 352.0, 352.0];
        assert!(
            encode_gif(&image.frames, rect, (352, 352), image.repeat)
                .unwrap()
                .is_none()
        );
        let result = crop_image(&image, rect, (512, 512), false).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(result.split_once(',').unwrap().1)
            .unwrap();
        assert!(bytes.len() <= MAX_GIF_BYTES);
        let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).unwrap();
        assert!(
            matches!(decoder.loop_count(), image::metadata::LoopCount::Finite(n) if n.get() == 2)
        );
        let frames = decoder.into_frames().collect_frames().unwrap();
        assert_eq!(frames.len(), image.frames.len());
        let (width, height) = frames[0].buffer().dimensions();
        assert_eq!(width, height);
        assert!(width < 352);
        for (before, after) in image.frames.iter().zip(&frames) {
            assert_eq!(before.delay(), after.delay());
            assert_eq!(after.buffer().dimensions(), (width, height));
        }
    }

    #[test]
    fn animated_banner_crop_does_not_upscale_the_original() {
        let image = decode(&data_url("image/gif", OPTIMIZED_GIF)).unwrap();
        let result = crop_image(&image, [0.0, 128.0, 384.0, 128.0], (1024, 341), true).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(result.split_once(',').unwrap().1)
            .unwrap();
        assert!(bytes.len() <= MAX_GIF_BYTES);
        let frames = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
            .unwrap()
            .into_frames()
            .collect_frames()
            .unwrap();
        assert_eq!(frames.len(), image.frames.len());
        assert!(frames[0].buffer().width() <= 384);
        assert!(frames[0].buffer().height() <= 128);
    }

    #[test]
    fn gif_writer_stops_before_exceeding_the_limit() {
        use std::io::Write;
        let mut bytes = vec![0; MAX_GIF_BYTES - 1];
        let mut writer = GifOutput {
            bytes: &mut bytes,
            exceeded: false,
        };
        writer.write_all(&[1]).unwrap();
        assert!(!writer.exceeded);
        assert!(writer.write_all(&[2]).is_err());
        assert!(writer.exceeded);
        assert_eq!(bytes.len(), MAX_GIF_BYTES);
    }

    #[test]
    fn transparent_gif_frames_clear_the_previous_image_after_cropping() {
        let mut input = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut input);
            for right in [false, true] {
                let pixels = RgbaImage::from_fn(8, 8, |x, _| {
                    if (x >= 4) == right {
                        image::Rgba([255, 0, 0, 255])
                    } else {
                        image::Rgba([0, 0, 0, 0])
                    }
                });
                encoder.encode_frame(image::Frame::new(pixels)).unwrap();
            }
        }
        let image = decode(&data_url("image/gif", &input)).unwrap();
        let result = crop_image(&image, [1.0, 1.0, 6.0, 6.0], (512, 512), false).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(result.split_once(',').unwrap().1)
            .unwrap();
        let frames = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
            .unwrap()
            .into_frames()
            .collect_frames()
            .unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].buffer().get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(frames[0].buffer().get_pixel(5, 0)[3], 0);
        assert_eq!(frames[1].buffer().get_pixel(0, 0)[3], 0);
        assert_eq!(frames[1].buffer().get_pixel(5, 0).0, [255, 0, 0, 255]);
    }

    #[test]
    fn animated_profile_crop_keeps_frames_timing_and_selected_region() {
        let mut input = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut input);
            encoder
                .set_repeat(image::codecs::gif::Repeat::Infinite)
                .unwrap();
            for (color, delay) in [([255, 0, 0, 255], 100), ([0, 255, 0, 255], 250)] {
                let pixels = RgbaImage::from_fn(8, 4, |x, _| {
                    if x < 4 {
                        image::Rgba(color)
                    } else {
                        image::Rgba([0, 0, 255, 255])
                    }
                });
                encoder
                    .encode_frame(image::Frame::from_parts(
                        pixels,
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(delay, 1),
                    ))
                    .unwrap();
            }
        }
        let source = data_url("image/gif", &input);
        let decoded = decode(&source).unwrap();
        assert_eq!(decoded.preview, source);
        for (size, expected) in [((32, 32), (4, 4)), ((96, 32), (4, 1))] {
            let cropped = crop_image(&decoded, [0.0, 0.0, 4.0, 4.0], size, true).unwrap();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(cropped.split_once(',').unwrap().1)
                .unwrap();
            let frames = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
                .unwrap()
                .into_frames()
                .collect_frames()
                .unwrap();
            assert_eq!(frames.len(), 2);
            assert_eq!(frames[0].buffer().dimensions(), expected);
            assert_eq!(frames[0].buffer().get_pixel(0, 0).0, [255, 0, 0, 255]);
            assert_eq!(frames[1].buffer().get_pixel(0, 0).0, [0, 255, 0, 255]);
            assert_eq!(frames[0].delay().numer_denom_ms(), (100, 1));
            assert_eq!(frames[1].delay().numer_denom_ms(), (250, 1));
        }
    }

    fn pixels_from_url(url: &str) -> image::DynamicImage {
        let payload = url.split_once(',').unwrap().1;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap();
        image::load_from_memory(&bytes).unwrap()
    }

    #[test]
    fn crop_uses_selected_region_and_preserves_avatar_alpha() {
        let pixels = RgbaImage::from_fn(12, 4, |x, _| {
            if x < 4 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 128])
            }
        });
        let url = crop(&pixels, [4.0, 0.0, 4.0, 4.0], (512, 512), false).unwrap();
        let decoded = pixels_from_url(&url).into_rgba8();
        assert_eq!(decoded.dimensions(), (512, 512));
        assert_eq!(decoded.get_pixel(256, 256).0, [0, 0, 255, 128]);
    }

    #[test]
    fn banner_exports_jpeg_and_flattens_transparency_to_white() {
        let pixels = RgbaImage::from_pixel(6, 2, image::Rgba([0, 0, 0, 0]));
        let url = crop(&pixels, [0.0, 0.0, 6.0, 2.0], (1024, 341), true).unwrap();
        assert!(url.starts_with("data:image/jpeg;base64,"));
        let decoded = pixels_from_url(&url).into_rgb8();
        assert_eq!(decoded.dimensions(), (1024, 341));
        assert!(decoded.get_pixel(512, 170).0.iter().all(|v| *v >= 254));
    }

    #[test]
    fn transparent_colors_do_not_bleed_into_resized_edges() {
        let pixels = RgbaImage::from_fn(4, 4, |x, _| {
            if x < 2 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 0])
            }
        });
        let url = crop(&pixels, [0.0, 0.0, 4.0, 4.0], (64, 64), false).unwrap();
        let resized = pixels_from_url(&url).into_rgba8();
        assert!(
            resized
                .pixels()
                .any(|pixel| pixel[3] > 16 && pixel[3] < 240)
        );
        for pixel in resized.pixels().filter(|pixel| pixel[3] > 16) {
            assert_eq!(pixel[2], 0);
            assert!(pixel[0] >= 250);
        }
    }

    #[test]
    fn decode_rejects_corrupt_data_and_crop_rejects_invalid_coordinates() {
        assert!(decode("data:image/png;base64,broken").is_err());
        assert!(decode("https://example.com/avatar.png").is_err());
        let pixels = RgbaImage::new(4, 4);
        assert!(crop(&pixels, [f64::NAN, 0.0, 4.0, 4.0], (512, 512), false).is_err());
        assert!(crop(&pixels, [3.0, 0.0, 4.0, 4.0], (512, 512), false).is_err());
    }

    #[test]
    fn decode_prepares_a_stable_preview() {
        let pixels = RgbaImage::from_pixel(8, 6, image::Rgba([25, 50, 75, 255]));
        let decoded = decode(&png_url(&pixels).unwrap()).unwrap();
        assert_eq!(decoded.pixels, pixels);
        assert_eq!(pixels_from_url(&decoded.preview).into_rgba8(), pixels);
    }

    #[test]
    fn preview_is_bounded_without_reducing_crop_resolution() {
        let pixels = RgbaImage::from_pixel(2048, 1024, image::Rgba([25, 50, 75, 255]));
        let decoded = decode(&png_url(&pixels).unwrap()).unwrap();
        assert_eq!(decoded.pixels.dimensions(), (2048, 1024));
        let preview = pixels_from_url(&decoded.preview);
        assert_eq!((preview.width(), preview.height()), (1024, 512));
    }

    #[test]
    fn supported_upload_formats_decode_natively() {
        let pixels = RgbaImage::from_pixel(8, 6, image::Rgba([25, 50, 75, 255]));
        for (format, mime) in [
            (image::ImageFormat::Png, "image/png"),
            (image::ImageFormat::Jpeg, "image/jpeg"),
            (image::ImageFormat::Gif, "image/gif"),
            (image::ImageFormat::WebP, "image/webp"),
        ] {
            let mut bytes = Cursor::new(Vec::new());
            let image = image::DynamicImage::ImageRgba8(pixels.clone());
            if format == image::ImageFormat::Jpeg {
                image.to_rgb8().write_to(&mut bytes, format).unwrap();
            } else {
                image.write_to(&mut bytes, format).unwrap();
            }
            let decoded = decode(&data_url(mime, bytes.get_ref())).unwrap();
            assert_eq!(decoded.pixels.dimensions(), (8, 6), "{mime}");
        }
    }

    #[test]
    fn jpeg_orientation_matches_the_preview_and_crop() {
        let pixels = image::RgbImage::from_pixel(8, 6, image::Rgb([25, 50, 75]));
        let mut bytes = Cursor::new(Vec::new());
        pixels
            .write_to(&mut bytes, image::ImageFormat::Jpeg)
            .unwrap();
        let exif = [
            b'E', b'x', b'i', b'f', 0, 0, b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 1, 3, 0, 1, 0,
            0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
        ];
        let mut oriented = vec![0xff, 0xd8, 0xff, 0xe1, 0, 34];
        oriented.extend_from_slice(&exif);
        oriented.extend_from_slice(&bytes.get_ref()[2..]);
        let decoded = decode(&data_url("image/jpeg", &oriented)).unwrap();
        assert_eq!(decoded.pixels.dimensions(), (6, 8));
        let preview = pixels_from_url(&decoded.preview);
        assert_eq!((preview.width(), preview.height()), (6, 8));
    }
}
