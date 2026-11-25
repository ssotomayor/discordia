//! `notification-avatars/<pubkey>.png`: the still a notification draws for a
//! sender. Android cannot animate one, and a cold push cannot fetch or decode.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

pub const DIR: &str = "notification-avatars";
const INDEX: &str = "index.json";
const SIDE: u32 = 128;
const MAX_INPUT_SIDE: u32 = 4096;
const MAX_DECODE_BYTES: u64 = 96 * 1024 * 1024;

/// One peer's picture as the app holds it. Equal by address alone: comparing
/// the data URLs on every state change would cost more than the write it saves.
#[derive(Debug, Clone)]
pub struct Source {
    pub pubkey: String,
    pub address: Option<String>,
    pub data: Option<Arc<String>>,
}

impl PartialEq for Source {
    fn eq(&self, other: &Self) -> bool {
        self.pubkey == other.pubkey
            && self.address == other.address
            && self.data.is_some() == other.data.is_some()
    }
}

impl Eq for Source {}

/// First frame, centre-cropped square, as PNG.
pub fn still(data_url: &str) -> Option<Vec<u8>> {
    let (_, bytes) = crate::nostr::blossom::split_data_url(data_url)?;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_INPUT_SIDE);
    limits.max_image_height = Some(MAX_INPUT_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let picture =
        reader
            .decode()
            .ok()?
            .resize_to_fill(SIDE, SIDE, image::imageops::FilterType::Triangle);
    let mut out = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

fn file_name(pubkey: &str) -> String {
    format!("{pubkey}.png")
}

/// Brings the directory to `sources`. A picture not in memory keeps the file
/// made from the same address; one whose address changed or went is removed.
pub fn sync(dir: &Path, sources: &[Source]) {
    let root = dir.join(DIR);
    if std::fs::create_dir_all(&root).is_err() {
        return;
    }
    let mut index: BTreeMap<String, String> = std::fs::read(root.join(INDEX))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let mut keep = BTreeSet::new();
    for source in sources {
        if !crate::protocol::is_canonical_pubkey(&source.pubkey) {
            continue;
        }
        let Some(address) = &source.address else {
            continue;
        };
        if index.get(&source.pubkey) == Some(address) {
            keep.insert(source.pubkey.clone());
            continue;
        }
        let Some(png) = source.data.as_deref().and_then(|data| still(data)) else {
            continue;
        };
        if crate::identity::write_private_bytes(&root.join(file_name(&source.pubkey)), &png).is_ok()
        {
            index.insert(source.pubkey.clone(), address.clone());
            keep.insert(source.pubkey.clone());
        }
    }
    index.retain(|pubkey, _| keep.contains(pubkey));
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let held = name
                .strip_suffix(".png")
                .is_some_and(|pubkey| keep.contains(pubkey));
            if name != INDEX && !held {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    if let Ok(json) = serde_json::to_string(&index) {
        let _ = crate::identity::write_private(&root.join(INDEX), &json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn data_url(format: image::ImageFormat, mime: &str, w: u32, h: u32) -> String {
        let picture = image::RgbaImage::from_pixel(w, h, image::Rgba([200, 30, 30, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(picture)
            .write_to(&mut out, format)
            .unwrap();
        format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(out.into_inner())
        )
    }

    fn source(n: u8, address: &str, data: Option<String>) -> Source {
        Source {
            pubkey: format!("{n:02x}").repeat(32),
            address: Some(address.into()),
            data: data.map(Arc::new),
        }
    }

    #[test]
    fn a_gif_becomes_a_small_square_still() {
        let png = still(&data_url(image::ImageFormat::Gif, "image/gif", 300, 200)).unwrap();
        let decoded = image::load_from_memory(&png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (SIDE, SIDE));
    }

    #[test]
    fn an_oversized_picture_is_refused_before_it_is_decoded() {
        let huge = data_url(image::ImageFormat::Png, "image/png", MAX_INPUT_SIDE + 1, 1);
        assert!(still(&huge).is_none());
    }

    #[test]
    fn the_directory_follows_the_peers_and_keeps_what_memory_dropped() {
        let dir = std::env::temp_dir().join(format!("discordia-avatars-{}", uuid::Uuid::new_v4()));
        let picture = data_url(image::ImageFormat::Png, "image/png", 64, 64);
        let (a, b) = (
            format!("{:02x}", 1).repeat(32),
            format!("{:02x}", 2).repeat(32),
        );
        let file = |key: &str| dir.as_path().join(DIR).join(file_name(key));

        sync(
            dir.as_path(),
            &[
                source(1, "media:a.png", Some(picture.clone())),
                source(2, "media:b.png", Some(picture.clone())),
            ],
        );
        assert!(file(&a).is_file() && file(&b).is_file());

        sync(
            dir.as_path(),
            &[
                source(1, "media:a.png", None),
                source(2, "media:c.png", None),
            ],
        );
        assert!(file(&a).is_file(), "same address, bytes evicted: keep");
        assert!(
            !file(&b).is_file(),
            "new address, no bytes: the old face is wrong"
        );

        sync(dir.as_path(), &[]);
        assert!(!file(&a).is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
