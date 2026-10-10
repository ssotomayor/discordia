use std::io::{Cursor, Read};
use std::path::Path;

use base64::Engine as _;
use image::{ImageDecoder, ImageEncoder, ImageReader};

pub const MAX_BYTES: usize = 2_000_000;
const MAX_PIXELS: u64 = 16_000_000;
const TOO_LARGE: &str = "Image too large (max 2 MB).";

pub fn read_file(path: &Path) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|_| "Couldn't read that file.")?;
    let metadata = file.metadata().map_err(|_| "Couldn't read that file.")?;
    if !metadata.is_file() {
        return Err("That's not an image file.".into());
    }
    if metadata.len() > MAX_BYTES as u64 {
        return Err(TOO_LARGE.into());
    }
    let mut bytes = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Couldn't read that file.")?;
    from_bytes(&bytes)
}

fn from_bytes(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() > MAX_BYTES {
        return Err(TOO_LARGE.into());
    }
    let format = image::guess_format(bytes).map_err(|_| "That's not a supported image.")?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(MAX_PIXELS * 8);
    reader.limits(limits);
    let decoder = reader
        .into_decoder()
        .map_err(|_| "Couldn't open that image.")?;
    let (width, height) = decoder.dimensions();
    check_dimensions(u64::from(width), u64::from(height))?;
    image::DynamicImage::from_decoder(decoder).map_err(|_| "Couldn't decode that image.")?;
    Ok(data_url(format.to_mime_type(), bytes))
}

pub fn read_clipboard() -> Result<String, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|_| "Clipboard unavailable.")?;
    let image = clipboard
        .get_image()
        .map_err(|_| "Couldn't read an image from the clipboard.")?;
    rgba_png(image.width, image.height, &image.bytes)
}

fn check_dimensions(width: u64, height: u64) -> Result<(), String> {
    if width == 0 || height == 0 || width > 8192 || height > 8192 || width * height > MAX_PIXELS {
        return Err("That image has too many pixels.".into());
    }
    Ok(())
}

fn rgba_png(width: usize, height: usize, pixels: &[u8]) -> Result<String, String> {
    check_dimensions(width as u64, height as u64)?;
    if width.checked_mul(height).and_then(|n| n.checked_mul(4)) != Some(pixels.len()) {
        return Err("Invalid clipboard image.".into());
    }
    let mut bytes = BoundedBytes(Vec::new());
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(
            pixels,
            width as u32,
            height as u32,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|_| TOO_LARGE)?;
    Ok(data_url("image/png", &bytes.0))
}

struct BoundedBytes(Vec<u8>);

impl std::io::Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_BYTES - self.0.len() {
            return Err(std::io::Error::other(TOO_LARGE));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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

    #[test]
    fn animated_chat_images_keep_the_original_bytes() {
        let bytes = include_bytes!("../tests/fixtures/optimized-profile.gif");
        assert!(bytes.len() < 1_000_000);
        let url = from_bytes(bytes).unwrap();
        assert!(url.starts_with("data:image/gif;base64,"));
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(url.split_once(',').unwrap().1)
                .unwrap(),
            bytes
        );
    }

    #[test]
    fn rejects_non_images_and_oversized_input() {
        assert!(from_bytes(b"not an image").is_err());
        assert_eq!(from_bytes(&vec![0; MAX_BYTES + 1]), Err(TOO_LARGE.into()));
        assert!(rgba_png(1, 1, &[0; 3]).is_err());
        assert!(rgba_png(usize::MAX, 2, &[]).is_err());
    }

    #[test]
    fn clipboard_png_preserves_alpha_and_file_bytes() {
        let url = rgba_png(2, 1, &[255, 0, 0, 255, 0, 255, 0, 64]).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(url.split_once(',').unwrap().1)
            .unwrap();
        assert_eq!(from_bytes(&bytes).unwrap(), url);
        assert_eq!(
            image::load_from_memory(&bytes)
                .unwrap()
                .into_rgba8()
                .as_raw(),
            &[255, 0, 0, 255, 0, 255, 0, 64]
        );
        assert!(from_bytes(&bytes[..bytes.len() / 2]).is_err());
    }

    #[test]
    fn png_writer_never_exceeds_upload_limit() {
        use std::io::Write;
        let mut output = BoundedBytes(vec![0; MAX_BYTES - 1]);
        assert!(output.write_all(&[1, 2]).is_err());
        assert_eq!(output.0.len(), MAX_BYTES - 1);
        assert!(check_dimensions(8192, 8192).is_err());
    }

    #[test]
    fn native_file_read_is_bounded_and_uses_content_type() {
        let path =
            std::env::temp_dir().join(format!("discordia-image-{}.txt", uuid::Uuid::new_v4()));
        let url = rgba_png(1, 1, &[5, 6, 7, 255]).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(url.split_once(',').unwrap().1)
            .unwrap();
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(read_file(&path).unwrap(), url);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len((MAX_BYTES + 1) as u64)
            .unwrap();
        assert_eq!(read_file(&path), Err(TOO_LARGE.into()));
        std::fs::remove_file(&path).unwrap();
        assert!(read_file(&path).is_err());
    }
}
