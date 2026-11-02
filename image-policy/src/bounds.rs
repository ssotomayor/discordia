//! Read from headers, never decoded: compression packs billions of pixels into
//! 2 MB, and every viewer's webview would allocate them.

/// Room for a 200 MP phone photo (16320×12240).
pub const MAX_SIDE: u32 = 16384;
pub const MAX_PIXELS: u64 = 1 << 28;
pub const MAX_FRAMES: u64 = 2000;
/// Canvas pixels times frames: what a viewer composites per loop.
pub const MAX_PIXEL_FRAMES: u64 = 1 << 30;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Bounds {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
}

pub fn check(bytes: &[u8]) -> Result<Bounds, String> {
    let bounds = read(bytes).ok_or_else(|| "Image headers are malformed.".to_string())?;
    if bounds.width == 0 || bounds.height == 0 {
        return Err("Image has no pixels.".into());
    }
    if bounds.width > MAX_SIDE || bounds.height > MAX_SIDE {
        return Err(format!("Image exceeds {MAX_SIDE} pixels on a side."));
    }
    let area = u64::from(bounds.width) * u64::from(bounds.height);
    if area > MAX_PIXELS {
        return Err("Image has too many pixels.".into());
    }
    if bounds.frames > MAX_FRAMES {
        return Err(format!("Image has more than {MAX_FRAMES} frames."));
    }
    if area.saturating_mul(bounds.frames.max(1)) > MAX_PIXEL_FRAMES {
        return Err("Animation is too large to play.".into());
    }
    Ok(bounds)
}

fn read(bytes: &[u8]) -> Option<Bounds> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        png(bytes)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        jpeg(bytes)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        gif(bytes)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        webp(bytes)
    } else if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        avif(bytes)
    } else {
        None
    }
}

fn be16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}
fn le16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn le24(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 3)?;
    Some(u32::from(s[0]) | u32::from(s[1]) << 8 | u32::from(s[2]) << 16)
}

/// The larger of the declared (`acTL`) and actual (`fcTL`) counts: decoders differ on which they trust.
fn png(b: &[u8]) -> Option<Bounds> {
    if b.get(12..16)? != b"IHDR" {
        return None;
    }
    let mut out = Bounds {
        width: be32(b, 16)?,
        height: be32(b, 20)?,
        frames: 1,
    };
    let (mut at, mut fctl, mut declared) = (8usize, 0u64, 0u64);
    while let (Some(len), Some(kind)) = (be32(b, at), b.get(at + 4..at + 8)) {
        match kind {
            b"acTL" => declared = u64::from(be32(b, at + 8)?),
            b"fcTL" => fctl += 1,
            b"IEND" => break,
            _ => {}
        }
        let Some(next) = at.checked_add(12).and_then(|n| n.checked_add(len as usize)) else {
            break;
        };
        at = next;
    }
    out.frames = out.frames.max(fctl).max(declared);
    Some(out)
}

fn jpeg(b: &[u8]) -> Option<Bounds> {
    let mut at = 2usize;
    loop {
        while *b.get(at)? != 0xFF {
            at += 1;
        }
        while *b.get(at)? == 0xFF {
            at += 1;
        }
        let marker = *b.get(at)?;
        at += 1;
        if matches!(marker, 0x01 | 0xD0..=0xD8) {
            continue;
        }
        if matches!(marker, 0xD9 | 0xDA) {
            return None;
        }
        let len = be16(b, at)? as usize;
        if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return Some(Bounds {
                width: be16(b, at + 5)?,
                height: be16(b, at + 3)?,
                frames: 1,
            });
        }
        at = at.checked_add(len)?;
    }
}

/// A frame drawn past the logical screen grows what some decoders allocate.
fn gif(b: &[u8]) -> Option<Bounds> {
    let mut out = Bounds {
        width: le16(b, 6)?,
        height: le16(b, 8)?,
        frames: 0,
    };
    let table = |packed: u8| {
        if packed & 0x80 != 0 {
            3usize << ((packed & 7) + 1)
        } else {
            0
        }
    };
    let mut at = 13 + table(*b.get(10)?);
    let skip_blocks = |mut at: usize| -> Option<usize> {
        loop {
            let len = *b.get(at)? as usize;
            at += 1 + len;
            if len == 0 {
                return Some(at);
            }
        }
    };
    // A truncated file still plays the frames before the cut, as viewers do.
    while let Some(&tag) = b.get(at) {
        match tag {
            0x2C => {
                let (x, y) = (le16(b, at + 1)?, le16(b, at + 3)?);
                let (w, h) = (le16(b, at + 5)?, le16(b, at + 7)?);
                out.width = out.width.max(x + w);
                out.height = out.height.max(y + h);
                out.frames += 1;
                at += 10 + table(*b.get(at + 9)?) + 1;
                match skip_blocks(at) {
                    Some(next) => at = next,
                    None => break,
                }
            }
            0x21 => match skip_blocks(at + 2) {
                Some(next) => at = next,
                None => break,
            },
            _ => break,
        }
    }
    out.frames = out.frames.max(1);
    Some(out)
}

fn webp(b: &[u8]) -> Option<Bounds> {
    let mut out = Bounds::default();
    let (mut at, mut frames) = (12usize, 0u64);
    while b.len() >= at + 8 {
        let kind = &b[at..at + 4];
        let len = u32::from_le_bytes(b[at + 4..at + 8].try_into().ok()?) as usize;
        let body = at + 8;
        match kind {
            b"VP8X" => {
                out.width = le24(b, body + 4)? + 1;
                out.height = le24(b, body + 7)? + 1;
            }
            b"ANMF" => {
                frames += 1;
                let (x, y) = (le24(b, body)? * 2, le24(b, body + 3)? * 2);
                out.width = out.width.max(x + le24(b, body + 6)? + 1);
                out.height = out.height.max(y + le24(b, body + 9)? + 1);
            }
            b"VP8 " if out.width == 0 => {
                if b.get(body + 3..body + 6)? != [0x9D, 0x01, 0x2A] {
                    return None;
                }
                out.width = le16(b, body + 6)? & 0x3FFF;
                out.height = le16(b, body + 8)? & 0x3FFF;
            }
            b"VP8L" if out.width == 0 => {
                if *b.get(body)? != 0x2F {
                    return None;
                }
                let bits = u32::from_le_bytes(b.get(body + 1..body + 5)?.try_into().ok()?);
                out.width = (bits & 0x3FFF) + 1;
                out.height = ((bits >> 14) & 0x3FFF) + 1;
            }
            _ => {}
        }
        at = body.checked_add(len)?.checked_add(len & 1)?;
    }
    out.frames = frames.max(1);
    Some(out)
}

fn avif(b: &[u8]) -> Option<Bounds> {
    let mut out = Bounds::default();
    let mut frames = 0u64;
    walk(b, 0, b.len(), 0, &mut out, &mut frames)?;
    out.frames = frames.max(1);
    Some(out)
}

fn walk(
    b: &[u8],
    mut at: usize,
    end: usize,
    depth: u8,
    out: &mut Bounds,
    frames: &mut u64,
) -> Option<()> {
    if depth > 8 {
        return None;
    }
    while at + 8 <= end {
        let mut size = be32(b, at)? as usize;
        let kind = b.get(at + 4..at + 8)?;
        let mut header = 8;
        if size == 1 {
            size = usize::try_from(u64::from_be_bytes(b.get(at + 8..at + 16)?.try_into().ok()?))
                .ok()?;
            header = 16;
        } else if size == 0 {
            size = end - at;
        }
        let next = at.checked_add(size)?;
        if size < header || next > end {
            return None;
        }
        let body = at + header;
        match kind {
            b"meta" => walk(b, body + 4, next, depth + 1, out, frames)?,
            b"iprp" | b"ipco" | b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => {
                walk(b, body, next, depth + 1, out, frames)?
            }
            b"ispe" => {
                out.width = out.width.max(be32(b, body + 4)?);
                out.height = out.height.max(be32(b, body + 8)?);
            }
            b"tkhd" => {
                out.width = out.width.max(be32(b, next - 8)? >> 16);
                out.height = out.height.max(be32(b, next - 4)? >> 16);
            }
            b"stsz" => *frames += u64::from(be32(b, body + 8)?),
            _ => {}
        }
        at = next;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;

    fn encoded(format: image::ImageFormat, w: u32, h: u32) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        let img = image::DynamicImage::ImageRgba8(RgbaImage::new(w, h));
        let img = if format == image::ImageFormat::Jpeg {
            image::DynamicImage::ImageRgb8(img.to_rgb8())
        } else {
            img
        };
        img.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    #[test]
    fn real_files_report_their_headers() {
        for format in [
            image::ImageFormat::Png,
            image::ImageFormat::Jpeg,
            image::ImageFormat::Gif,
            image::ImageFormat::WebP,
        ] {
            let bounds = check(&encoded(format, 37, 21)).unwrap();
            assert_eq!(
                (bounds.width, bounds.height, bounds.frames),
                (37, 21, 1),
                "{format:?}"
            );
        }
    }

    #[test]
    fn an_animated_gif_counts_its_frames() {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for _ in 0..3 {
                encoder
                    .encode_frame(image::Frame::new(RgbaImage::new(4, 4)))
                    .unwrap();
            }
        }
        assert_eq!(check(&bytes).unwrap().frames, 3);
    }

    fn gif(width: u16, height: u16, frames: usize) -> Vec<u8> {
        let mut b = b"GIF89a".to_vec();
        b.extend(width.to_le_bytes());
        b.extend(height.to_le_bytes());
        b.extend([0, 0, 0]);
        for _ in 0..frames {
            b.push(0x2C);
            b.extend([0, 0, 0, 0, 1, 0, 1, 0, 0]);
            b.extend([2, 2, 0x4C, 0x01, 0]);
        }
        b.push(0x3B);
        b
    }

    #[test]
    fn a_tiny_gif_cannot_claim_a_huge_canvas_or_endless_frames() {
        assert!(check(&gif(64, 64, 1)).is_ok());
        assert!(check(&gif(65535, 65535, 1)).is_err());
        assert!(check(&gif(64, 64, MAX_FRAMES as usize + 1)).is_err());
        assert!(check(&gif(4096, 4096, 100)).is_err());
    }

    #[test]
    fn a_gif_frame_past_the_screen_grows_the_canvas() {
        let mut b = gif(16, 16, 1);
        b[13 + 5..13 + 9].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(check(&b).is_err());
    }

    #[test]
    fn a_png_or_apng_is_held_to_its_header() {
        let mut big = encoded(image::ImageFormat::Png, 8, 8);
        big[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        big[20..24].copy_from_slice(&20_000u32.to_be_bytes());
        assert!(check(&big).is_err());

        let mut apng = encoded(image::ImageFormat::Png, 8, 8);
        let mut actl = 8u32.to_be_bytes().to_vec();
        actl.extend(b"acTL");
        actl.extend(5000u32.to_be_bytes());
        actl.extend(0u32.to_be_bytes());
        actl.extend([0; 4]);
        apng.splice(33..33, actl);
        assert!(check(&apng).is_err());
    }

    #[test]
    fn a_webp_canvas_is_checked() {
        let mut b = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        b.extend(10u32.to_le_bytes());
        b.extend([0; 4]);
        b.extend(&(19_999u32.to_le_bytes()[..3]));
        b.extend(&(99u32.to_le_bytes()[..3]));
        assert!(check(&b).is_err());
    }

    fn avif(width: u32) -> Vec<u8> {
        let boxed = |kind: &[u8], body: Vec<u8>| {
            let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
            b.extend(kind);
            b.extend(body);
            b
        };
        let mut ispe = vec![0; 4];
        ispe.extend(width.to_be_bytes());
        ispe.extend(100u32.to_be_bytes());
        let mut meta = vec![0; 4];
        meta.extend(boxed(b"iprp", boxed(b"ipco", boxed(b"ispe", ispe))));
        let mut b = boxed(b"ftyp", b"avif\0\0\0\0avif".to_vec());
        b.extend(boxed(b"meta", meta));
        b
    }

    #[test]
    fn an_avif_item_size_is_read_from_ispe() {
        assert_eq!(check(&avif(200)).unwrap().width, 200);
        assert!(check(&avif(20_000)).is_err());
    }

    #[test]
    fn unknown_or_empty_headers_are_refused() {
        assert!(check(b"GIF89a").is_err());
        assert!(check(b"nothing").is_err());
        assert!(check(&avif(0)).is_err());
    }
}
