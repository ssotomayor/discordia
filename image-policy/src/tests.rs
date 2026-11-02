use super::*;

fn png(w: u32, h: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(w, h, image::Rgba([120, 80, 40, 96]));
    let mut encoded = Cursor::new(Vec::new());
    image.write_to(&mut encoded, ImageFormat::Png).unwrap();
    encoded.into_inner()
}

#[test]
fn preserves_small_originals_and_alpha_without_jpeg_conversion() {
    let small = png(64, 48);
    assert!(create(&small).unwrap().is_none());
    let original = png(960, 640);
    let copy = original.clone();
    let preview = create(&original).unwrap().unwrap();
    assert_eq!(preview.mime, "image/png");
    let decoded = image::load_from_memory(&preview.bytes).unwrap().to_rgba8();
    assert_eq!(decoded.dimensions(), (640, 427));
    assert_eq!(decoded.get_pixel(100, 100).0[3], 96);
    assert_eq!(original, copy);
}

#[test]
fn validates_sender_previews_without_reencoding_them() {
    let preview = png(WIDTH, HEIGHT);
    assert!(validate_preview(&preview).is_ok());
    assert!(validate_preview(&png(WIDTH + 1, HEIGHT)).is_err());
    assert!(validate_preview(b"not an image").is_err());
    assert!(validate_preview(&vec![0; MAX_OUTPUT_BYTES + 1]).is_err());
}

#[test]
fn rotates_pixels_and_preserves_icc_profile() {
    let original = image::RgbImage::from_fn(1200, 800, |x, _| {
        if x < 600 {
            image::Rgb([240, 10, 10])
        } else {
            image::Rgb([10, 10, 240])
        }
    });
    let mut profile = vec![0u8; 132];
    profile[..4].copy_from_slice(&132u32.to_be_bytes());
    profile[16..20].copy_from_slice(b"RGB ");
    profile[36..40].copy_from_slice(b"acsp");
    let mut jpeg = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 92);
    encoder.set_icc_profile(profile.clone()).unwrap();
    encoder.encode_image(&original).unwrap();
    let exif = b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
    let mut oriented = jpeg[..2].to_vec();
    oriented.extend_from_slice(&[0xff, 0xe1]);
    oriented.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    oriented.extend_from_slice(exif);
    oriented.extend_from_slice(&jpeg[2..]);
    let preview = create(&oriented).unwrap().unwrap();
    assert_eq!(preview.mime, "image/jpeg");
    let decoded = image::load_from_memory(&preview.bytes).unwrap().to_rgb8();
    assert_eq!(decoded.dimensions(), (320, 480));
    assert!(decoded.get_pixel(160, 50).0[0] > 220);
    assert!(decoded.get_pixel(160, 430).0[2] > 220);
    let mut decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(&preview.bytes)).unwrap();
    assert_eq!(decoder.icc_profile().unwrap(), Some(profile));
    assert_eq!(
        decoder.orientation().unwrap(),
        image::metadata::Orientation::NoTransforms
    );
}

#[test]
fn gif_animation_is_never_flattened() {
    let mut bytes = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        encoder
            .set_repeat(image::codecs::gif::Repeat::Infinite)
            .unwrap();
        for color in [[240, 10, 10, 255], [10, 10, 240, 255]] {
            encoder
                .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                    960,
                    640,
                    image::Rgba(color),
                )))
                .unwrap();
        }
    }
    assert!(create(&bytes).unwrap().is_none());
}

fn chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_be_bytes());
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    let mut crc = !0u32;
    for byte in kind.iter().chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb88320 } else { 0 };
        }
    }
    output.extend_from_slice(&(!crc).to_be_bytes());
}

#[test]
fn apng_is_never_flattened() {
    use image::AnimationDecoder;
    let still = png(960, 640);
    let mut animated = still[..8].to_vec();
    let mut at = 8;
    let mut idat = Vec::new();
    while at < still.len() {
        let len = u32::from_be_bytes(still[at..at + 4].try_into().unwrap()) as usize;
        let kind = &still[at + 4..at + 8];
        let data = &still[at + 8..at + 8 + len];
        if kind == b"IHDR" {
            chunk(&mut animated, b"IHDR", data);
        } else if kind == b"IDAT" {
            idat.extend_from_slice(data);
        }
        at += len + 12;
    }
    chunk(&mut animated, b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]);
    for frame in 0..2u32 {
        let mut control = Vec::new();
        control.extend_from_slice(&(if frame == 0 { 0u32 } else { 1 }).to_be_bytes());
        control.extend_from_slice(&960u32.to_be_bytes());
        control.extend_from_slice(&640u32.to_be_bytes());
        control.extend_from_slice(&[0; 8]);
        control.extend_from_slice(&[0, 1, 0, 10, 0, 0]);
        chunk(&mut animated, b"fcTL", &control);
        if frame == 0 {
            chunk(&mut animated, b"IDAT", &idat);
        } else {
            let mut data = 2u32.to_be_bytes().to_vec();
            data.extend_from_slice(&idat);
            chunk(&mut animated, b"fdAT", &data);
        }
    }
    chunk(&mut animated, b"IEND", &[]);
    let decoder = image::codecs::png::PngDecoder::new(Cursor::new(&animated)).unwrap();
    assert_eq!(
        decoder
            .apng()
            .unwrap()
            .into_frames()
            .collect_frames()
            .unwrap()
            .len(),
        2
    );
    assert!(create(&animated).unwrap().is_none());
    assert!(validate_preview(&animated).is_err());
}

#[test]
fn rejects_invalid_and_oversized_inputs() {
    assert!(create(b"bad").is_err());
    assert!(create(&vec![0; MAX_INPUT_BYTES + 1]).is_err());
    assert!(create(&png(8192, 1)).unwrap().is_some());
}

#[test]
fn custom_png_transfer_metadata_retains_original_and_standard_gamma_can_resize() {
    let base = png(960, 640);
    for (gamma, original) in [(100000u32, true), (45455, false)] {
        let mut encoded = base[..33].to_vec();
        chunk(&mut encoded, b"gAMA", &gamma.to_be_bytes());
        encoded.extend_from_slice(&base[33..]);
        assert!(image::load_from_memory(&encoded).is_ok());
        assert_eq!(create(&encoded).unwrap().is_none(), original);
    }
    let mut hdr = base[..33].to_vec();
    chunk(&mut hdr, b"cICP", &[9, 16, 0, 1]);
    hdr.extend_from_slice(&base[33..]);
    assert!(create(&hdr).unwrap().is_none());
    let mut webp = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
        .encode(
            &vec![128; 960 * 640 * 4],
            960,
            640,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    assert!(create(&webp).unwrap().is_none());
}
