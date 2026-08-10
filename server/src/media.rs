//! Content-addressed blob storage for message images.
//!
//! Message attachments arrive on the wire as `data:image/...;base64,...` URLs
//! (unchanged protocol). Instead of storing megabytes of base64 inside message
//! rows, the gateway writes the decoded bytes here once (SHA-256 addressed, so
//! duplicates are free) and the DB stores a tiny `media:<hash>.<ext>` sentinel.
//! When a message is served, the sentinel is inlined back into a data URL —
//! clients (including rendezvous-proxied ones) need no changes. A direct
//! `GET /media/{file}` route also exists for future thin/web clients.
//!
//! GC of unreferenced blobs is deferred (content-addressing makes blobs shared,
//! so deletion needs refcounting — tracked in TODO.md).

use std::path::PathBuf;

use base64::Engine as _;
use sha2::{Digest, Sha256};

const SENTINEL: &str = "media:";

#[derive(Clone)]
pub struct MediaStore {
    dir: PathBuf,
}

impl MediaStore {
    pub fn open(dir: PathBuf) -> std::io::Result<MediaStore> {
        std::fs::create_dir_all(&dir)?;
        Ok(MediaStore { dir })
    }

    /// Store a `data:image/...;base64,...` URL as a blob file. Returns the
    /// DB sentinel (`media:<sha256>.<ext>`), or None if the input isn't a
    /// data URL we recognize (e.g. it's already an http URL — pass through).
    pub fn store_data_url(&self, data_url: &str) -> Option<String> {
        let rest = data_url.strip_prefix("data:")?;
        let (mime, payload) = rest.split_once(";base64,")?;
        let ext = ext_for_mime(mime)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .ok()?;
        let hash = hex::encode(Sha256::digest(&bytes));
        let name = format!("{hash}.{ext}");
        let path = self.dir.join(&name);
        if !path.exists() {
            // Write via temp + rename so a crash never leaves a torn blob
            // under its content address.
            let tmp = self.dir.join(format!(".{name}.tmp"));
            if std::fs::write(&tmp, &bytes).is_err() {
                return None;
            }
            if std::fs::rename(&tmp, &path).is_err() {
                let _ = std::fs::remove_file(&tmp);
                return None;
            }
        }
        Some(format!("{SENTINEL}{name}"))
    }

    /// Inline a stored sentinel back into a data URL for the wire. Non-sentinel
    /// values (http URLs, legacy data URLs) pass through untouched.
    pub fn inline(&self, stored: &str) -> Option<String> {
        let Some(name) = stored.strip_prefix(SENTINEL) else {
            return Some(stored.to_string());
        };
        let name = sanitize(name)?;
        let bytes = std::fs::read(self.dir.join(&name)).ok()?;
        let mime = mime_for_name(&name);
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        Some(format!("data:{mime};base64,{b64}"))
    }

    /// Raw bytes + mime for the HTTP route.
    pub fn read(&self, name: &str) -> Option<(Vec<u8>, &'static str)> {
        let name = sanitize(name)?;
        let bytes = std::fs::read(self.dir.join(&name)).ok()?;
        Some((bytes, mime_for_name(&name)))
    }
}

/// Only `<64 hex>.<short alnum ext>` filenames are ever served or read —
/// nothing else can exist in the blob dir, and this kills path traversal.
fn sanitize(name: &str) -> Option<String> {
    let (hash, ext) = name.split_once('.')?;
    if hash.len() == 64
        && hash.chars().all(|c| c.is_ascii_hexdigit())
        && (1..=5).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
    {
        Some(name.to_string())
    } else {
        None
    }
}

fn ext_for_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/avif" => Some("avif"),
        _ => None,
    }
}

fn mime_for_name(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, e)| e) {
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        _ => "application/octet-stream",
    }
}
