use image::{ImageDecoder, ImageEncoder, ImageFormat, ImageReader};
use std::io::Cursor;

pub const VERSION: u8 = 2;
pub const WIDTH: u32 = 640;
pub const HEIGHT: u32 = 480;
pub const MAX_INPUT_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

pub struct Preview {
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    limits
}

fn preserve_png_color(bytes: &[u8]) -> bool {
    let mut at = 8usize;
    while let Some(header) = bytes.get(at..).and_then(|tail| tail.get(..8)) {
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let Some(end) = at.checked_add(12).and_then(|start| start.checked_add(len)) else {
            return true;
        };
        let Some(data) = bytes.get(at + 8..end - 4) else {
            return true;
        };
        match &header[4..8] {
            b"IDAT" | b"IEND" => return false,
            b"cICP" | b"mDCv" | b"cLLi" => return true,
            b"gAMA" if data != 45455u32.to_be_bytes() => return true,
            b"cHRM"
                if ![31270u32, 32900, 64000, 33000, 30000, 60000, 15000, 6000]
                    .into_iter()
                    .flat_map(u32::to_be_bytes)
                    .eq(data.iter().copied()) =>
            {
                return true;
            }
            _ => {}
        }
        at = end;
    }
    false
}

pub fn create(bytes: &[u8]) -> Result<Option<Preview>, String> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err("Image exceeds preview input limit.".into());
    }
    let format = image::guess_format(bytes).map_err(|e| e.to_string())?;
    // Animated and unsupported codecs retain their original frames and metadata.
    if !matches!(format, ImageFormat::Jpeg | ImageFormat::Png) {
        return Ok(None);
    }
    let mut decoder: Box<dyn ImageDecoder + '_> = if format == ImageFormat::Png {
        // The encoder cannot carry arbitrary transfer curves, primaries or HDR metadata.
        if preserve_png_color(bytes) {
            return Ok(None);
        }
        let png = image::codecs::png::PngDecoder::with_limits(Cursor::new(bytes), limits())
            .map_err(|e| e.to_string())?;
        if png.is_apng().map_err(|e| e.to_string())? {
            return Ok(None);
        }
        Box::new(png)
    } else {
        let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
        reader.limits(limits());
        Box::new(reader.into_decoder().map_err(|e| e.to_string())?)
    };
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 16_000_000 {
        return Err("Too many image pixels.".into());
    }
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let (w, h) = if matches!(
        orientation,
        image::metadata::Orientation::Rotate90
            | image::metadata::Orientation::Rotate270
            | image::metadata::Orientation::Rotate90FlipH
            | image::metadata::Orientation::Rotate270FlipH
    ) {
        (height, width)
    } else {
        (width, height)
    };
    if w <= WIDTH && h <= HEIGHT {
        return Ok(None);
    }
    let profile = decoder.icc_profile().map_err(|e| e.to_string())?;
    if profile.as_ref().is_some_and(|p| {
        p.len() > 1024 * 1024 || (p.len() >= 20 && !matches!(&p[16..20], b"RGB " | b"GRAY"))
    }) {
        return Ok(None);
    }
    let mut image = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    image.apply_orientation(orientation);
    let preview = image.thumbnail(WIDTH, HEIGHT);
    let mut output = Vec::new();
    let mime = if format == ImageFormat::Png {
        let mut encoder = image::codecs::png::PngEncoder::new_with_quality(
            &mut output,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Adaptive,
        );
        if let Some(profile) = profile {
            encoder
                .set_icc_profile(profile)
                .map_err(|e| e.to_string())?;
        }
        encoder
            .write_image(
                preview.as_bytes(),
                preview.width(),
                preview.height(),
                preview.color().into(),
            )
            .map_err(|e| e.to_string())?;
        "image/png"
    } else {
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 92);
        if let Some(profile) = profile {
            encoder
                .set_icc_profile(profile)
                .map_err(|e| e.to_string())?;
        }
        encoder.encode_image(&preview).map_err(|e| e.to_string())?;
        "image/jpeg"
    };
    if output.len() > MAX_OUTPUT_BYTES {
        return Ok(None);
    }
    Ok(Some(Preview {
        mime,
        bytes: output,
    }))
}

#[cfg(test)]
mod tests;
