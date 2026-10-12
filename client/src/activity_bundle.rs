use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

use base64::Engine;
use sha2::{Digest, Sha256};

use crate::activity_window::{Bundle, BundleFile};

const MAX_FILES: usize = 256;
/// A zip is small on the wire and unbounded unpacked; this is the bound.
const MAX_UNPACKED_BYTES: u64 = 16 * 1024 * 1024;

/// The bytes the host sent, refused unless they hash to what the guild
/// approved: the host is trusted to store, not to choose.
pub fn verified_bytes(data_url: &str, sha256: &str) -> Result<Vec<u8>, String> {
    let (_, payload) = data_url
        .split_once(";base64,")
        .ok_or("the bundle arrived malformed")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|_| "the bundle arrived malformed")?;
    if hex::encode(Sha256::digest(&bytes)) != sha256 {
        return Err("the bundle does not match the approved manifest".into());
    }
    Ok(bytes)
}

pub fn unpack(bytes: &[u8]) -> Result<Bundle, String> {
    if !bytes.starts_with(b"PK\x03\x04") {
        let html = std::str::from_utf8(bytes).map_err(|_| "the page is not UTF-8")?;
        return Ok(Bundle::single_page(html));
    }
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("unreadable bundle: {e}"))?;
    if zip.len() > MAX_FILES {
        return Err(format!("a bundle holds at most {MAX_FILES} files"));
    }
    let mut files = HashMap::new();
    let mut budget = MAX_UNPACKED_BYTES;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| format!("unreadable bundle: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let path = clean_path(entry.name()).ok_or("a bundle path leaves the bundle")?;
        let mut data = Vec::new();
        let read = (&mut entry)
            .take(budget + 1)
            .read_to_end(&mut data)
            .map_err(|e| format!("unreadable bundle: {e}"))?;
        if read as u64 > budget {
            return Err("the bundle unpacks to more than 16 MiB".into());
        }
        budget -= read as u64;
        files.insert(
            path.clone(),
            BundleFile {
                mime: mime_for(&path).into(),
                bytes: Arc::from(data),
            },
        );
    }
    let files = strip_shared_folder(files);
    if !files.contains_key("index.html") {
        return Err("the bundle has no index.html".into());
    }
    Ok(Bundle { files })
}

fn clean_path(raw: &str) -> Option<String> {
    if raw.starts_with('/') || raw.contains('\\') || raw.contains(':') {
        return None;
    }
    let parts: Vec<&str> = raw
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if parts.is_empty() || parts.contains(&"..") {
        return None;
    }
    Some(parts.join("/"))
}

/// Zipping a folder puts everything under its name; the page is meant to be
/// that folder's `index.html`.
fn strip_shared_folder(files: HashMap<String, BundleFile>) -> HashMap<String, BundleFile> {
    if files.contains_key("index.html") {
        return files;
    }
    let first = files
        .keys()
        .next()
        .and_then(|k| k.split_once('/'))
        .map(|(dir, _)| format!("{dir}/"));
    match first {
        Some(prefix) if files.keys().all(|k| k.starts_with(&prefix)) => files
            .into_iter()
            .map(|(k, v)| (k[prefix.len()..].to_string(), v))
            .collect(),
        _ => files,
    }
}

fn mime_for(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()) {
        Some(e) => match e.as_str() {
            "html" | "htm" => "text/html; charset=utf-8",
            "js" | "mjs" => "text/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "json" => "application/json",
            "txt" => "text/plain; charset=utf-8",
            "svg" => "image/svg+xml",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "wasm" => "application/wasm",
            "mp3" => "audio/mpeg",
            "ogg" => "audio/ogg",
            "wav" => "audio/wav",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "ttf" => "font/ttf",
            _ => "application/octet-stream",
        },
        None => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut out);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, data) in entries {
                z.start_file(*name, opts).unwrap();
                z.write_all(data).unwrap();
            }
            z.finish().unwrap();
        }
        out.into_inner()
    }

    #[test]
    fn a_bundle_is_only_what_the_manifest_hashed() {
        let bytes = b"<p>hi</p>".to_vec();
        let sha = hex::encode(Sha256::digest(&bytes));
        let url = format!(
            "data:application/x-discordia-activity;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        assert_eq!(verified_bytes(&url, &sha).unwrap(), bytes);
        assert!(verified_bytes(&url, &"0".repeat(64)).is_err());
        assert!(verified_bytes("garbage", &sha).is_err());
    }

    #[test]
    fn a_zipped_folder_serves_its_index_and_assets() {
        let z = zip_of(&[
            ("game/index.html", b"<script src=app.js></script>"),
            ("game/app.js", b"1"),
        ]);
        let b = unpack(&z).unwrap();
        assert!(b.files.contains_key("index.html"));
        assert_eq!(b.files["app.js"].mime, "text/javascript; charset=utf-8");
    }

    #[test]
    fn a_path_cannot_leave_the_bundle() {
        for bad in ["../x", "/etc/passwd", "a/../../b", "C:/x", "a\\b"] {
            assert!(clean_path(bad).is_none(), "{bad}");
        }
        assert_eq!(clean_path("./a//b.js").as_deref(), Some("a/b.js"));
        assert!(unpack(&zip_of(&[("index.html", b"x"), ("../evil", b"x")])).is_err());
    }

    #[test]
    fn a_zip_bomb_is_refused() {
        let big = vec![0u8; (MAX_UNPACKED_BYTES + 1) as usize];
        let Err(e) = unpack(&zip_of(&[("index.html", &big)])) else {
            panic!("a bomb unpacked");
        };
        assert!(e.contains("16 MiB"), "{e}");
    }

    #[test]
    fn a_bundle_without_a_page_is_refused() {
        assert!(unpack(&zip_of(&[("app.js", b"1"), ("b.js", b"2")])).is_err());
        assert!(unpack(b"<!doctype html><p>one page</p>").is_ok());
    }
}
