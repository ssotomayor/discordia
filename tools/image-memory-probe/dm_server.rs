use base64::Engine as _;
use image::{AnimationDecoder, ImageDecoder, ImageFormat};
use std::{io::Cursor, path::Path, time::Instant};

fn unpack(url: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    Ok(base64::engine::general_purpose::STANDARD
        .decode(url.split_once(",").ok_or("data URL")?.1)?)
}

fn metadata(bytes: &[u8]) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let format = image::guess_format(bytes)?;
    let mut decoder = image::ImageReader::with_format(Cursor::new(bytes), format).into_decoder()?;
    let dimensions = decoder.dimensions();
    let orientation = format!("{:?}", decoder.orientation()?);
    let frames = if format == ImageFormat::Gif {
        image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?
            .into_frames()
            .collect_frames()?
            .len()
    } else {
        1
    };
    Ok(
        serde_json::json!({"bytes":bytes.len(),"mime":format.to_mime_type(),"width":dimensions.0,"height":dimensions.1,"orientation":orientation,"frames":frames}),
    )
}

fn store(
    root: &Path,
    case: &str,
    variant: &str,
    index: usize,
    bytes: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let ext = match image::guess_format(bytes)? {
        ImageFormat::Png => "png",
        ImageFormat::Gif => "gif",
        _ => "jpg",
    };
    let directory = root.join(case).join(variant);
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join(format!("high{index}.{ext}")), bytes)?;
    Ok(())
}

pub fn prepare(preview_server: bool) -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let root = Path::new(args.get(2).ok_or("output root required")?);
    std::fs::create_dir_all(root)?;
    let run: usize = args.get(3).ok_or("run required")?.parse()?;
    let existing = Path::new("target/image-memory/fixtures");
    let mut rows = Vec::new();
    for case in [
        "jpeg-medium",
        "jpeg-large",
        "png-alpha",
        "gif-large",
        "jpeg-exif",
    ] {
        let count = if matches!(case, "gif-large" | "jpeg-exif") {
            1
        } else {
            8
        };
        for index in 0..count {
            let input = match case {
                "jpeg-medium" => std::fs::read(existing.join(format!("chat{index}.jpg")))?,
                "jpeg-large" => std::fs::read(existing.join(format!("high{index}.jpg")))?,
                "png-alpha" => {
                    let file = root.join(format!("input-alpha{index}.png"));
                    if !file.exists() {
                        std::fs::create_dir_all(root)?;
                        let image = image::RgbaImage::from_fn(2560, 1440, |x, y| {
                            image::Rgba([
                                ((x / 8 + index as u32 * 11) % 256) as u8,
                                ((y / 8) % 256) as u8,
                                if x % 64 < 4 || y % 64 < 4 { 0 } else { 180 },
                                if x < 800 { 96 } else { 255 },
                            ])
                        });
                        image.save(&file)?;
                    }
                    std::fs::read(file)?
                }
                "gif-large" => {
                    let file = root.join("input-animated.gif");
                    if !file.exists() {
                        let mut encoder =
                            image::codecs::gif::GifEncoder::new(std::fs::File::create(&file)?);
                        encoder.set_repeat(image::codecs::gif::Repeat::Infinite)?;
                        for frame in 0..8 {
                            let image = image::RgbaImage::from_fn(960, 640, |x, y| {
                                if (x + frame * 80) % 960 < 160 && y < 480 {
                                    image::Rgba([255, 40, 20, 255])
                                } else {
                                    image::Rgba([20, 120, 240, 255])
                                }
                            });
                            encoder.encode_frame(image::Frame::from_parts(
                                image,
                                0,
                                0,
                                image::Delay::from_numer_denom_ms(100, 1),
                            ))?;
                        }
                    }
                    std::fs::read(file)?
                }
                _ => {
                    let jpeg = std::fs::read(existing.join("chat0.jpg"))?;
                    let exif =
                        b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
                    let mut bytes = jpeg[..2].to_vec();
                    bytes.extend_from_slice(&[0xff, 0xe1]);
                    bytes.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
                    bytes.extend_from_slice(exif);
                    bytes.extend_from_slice(&jpeg[2..]);
                    bytes
                }
            };
            let path = root.join("current-input");
            std::fs::write(&path, &input)?;
            let begin = Instant::now();
            let server = crate::chat_image::read_file_with_limit(
                &path,
                crate::protocol::DEFAULT_GUILD_UPLOAD_BYTES as usize,
            )?;
            let server_ms = begin.elapsed().as_secs_f64() * 1000.;
            let begin = Instant::now();
            let dm_original =
                crate::chat_image::read_file_with_limit(&path, crate::chat_image::MAX_DM_BYTES)?;
            let dm_send_ms = begin.elapsed().as_secs_f64() * 1000.;
            let dm_bytes = unpack(&dm_original)?;
            let begin = Instant::now();
            let validated = crate::chat_image::from_bytes_with_limit(
                &dm_bytes,
                crate::chat_image::MAX_DM_BYTES,
            )?;
            let thumb = crate::chat_image::thumbnail_data_url(&dm_bytes)?;
            let dm_receive_image_ms = begin.elapsed().as_secs_f64() * 1000.;
            drop(validated);
            let server_original = unpack(&server)?;
            let began = Instant::now();
            let server_bytes = if preview_server {
                unpack(&crate::chat_image::thumbnail_data_url(&server_original)?)?
            } else {
                server_original.clone()
            };
            let server_preview_ms = began.elapsed().as_secs_f64() * 1000.;
            let thumb_bytes = unpack(&thumb)?;
            store(root, case, "server", index, &server_bytes)?;
            store(root, case, "server-original", index, &server_original)?;
            store(root, case, "dm", index, &thumb_bytes)?;
            let mut row = serde_json::json!({"case":case,"index":index,"run":run,"input":metadata(&input)?,"server":metadata(&server_bytes)?,"serverOriginal":metadata(&server_original)?,"serverPreviewMs":server_preview_ms,"dmOriginal":metadata(&dm_bytes)?,"dmThumbnail":metadata(&thumb_bytes)?,"serverSendMs":server_ms,"dmSendMs":dm_send_ms,"dmColdImageWorkMs":dm_receive_image_ms,"serverUnchanged":server_original==input,"dmOriginalUnchanged":dm_bytes==input});
            if case == "jpeg-large" {
                let begin = Instant::now();
                let restricted = crate::chat_image::read_file_with_limit(&path, 2 * 1024 * 1024)?;
                let elapsed = begin.elapsed().as_secs_f64() * 1000.;
                let bytes = unpack(&restricted)?;
                store(root, case, "server-2mib", index, &bytes)?;
                row["server2MiB"] = metadata(&bytes)?;
                row["server2MiBSendMs"] = serde_json::json!(elapsed);
            }
            rows.push(row);
        }
    }
    std::fs::write(
        root.join(format!("preparation-{run}.json")),
        serde_json::to_vec_pretty(&rows)?,
    )?;
    Ok(())
}
