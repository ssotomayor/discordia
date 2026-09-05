use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use sha2::{Digest, Sha256};

const SENTINEL: &str = "media:";

pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepReport {
    pub kept: usize,
    pub too_young: usize,
    pub deleted: usize,
    pub freed_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    Unsupported,
    Full,
    Io,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StoreError::Unsupported => "unsupported image format (PNG, JPEG, GIF, WebP or AVIF)",
            StoreError::Full => "this server's media storage is full",
            StoreError::Io => "the server could not store the image",
        })
    }
}

/// `used` is what a quota can be checked against without a directory walk on
/// every upload; it is summed once at open and moved by writes and sweeps.
#[derive(Clone)]
pub struct MediaStore {
    dir: PathBuf,
    used: Arc<AtomicU64>,
    max_bytes: u64,
}

impl MediaStore {
    pub fn open(dir: PathBuf, max_bytes: u64) -> std::io::Result<MediaStore> {
        std::fs::create_dir_all(&dir)?;
        let used = std::fs::read_dir(&dir)?
            .flatten()
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum();
        Ok(MediaStore {
            dir,
            used: Arc::new(AtomicU64::new(used)),
            max_bytes,
        })
    }

    pub fn used_bytes(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }

    pub fn store_data_url(&self, data_url: &str) -> Result<String, StoreError> {
        let rest = data_url
            .strip_prefix("data:")
            .ok_or(StoreError::Unsupported)?;
        let (mime, payload) = rest.split_once(";base64,").ok_or(StoreError::Unsupported)?;
        let ext = ext_for_mime(mime).ok_or(StoreError::Unsupported)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|_| StoreError::Unsupported)?;
        if !looks_like(ext, &bytes) {
            return Err(StoreError::Unsupported);
        }
        let hash = hex::encode(Sha256::digest(&bytes));
        let name = format!("{hash}.{ext}");
        let path = self.dir.join(&name);
        if path.exists() {
            return Ok(format!("{SENTINEL}{name}"));
        }
        let len = bytes.len() as u64;
        if self.used_bytes().saturating_add(len) > self.max_bytes {
            return Err(StoreError::Full);
        }
        static TMP: AtomicU64 = AtomicU64::new(0);
        let tmp = self.dir.join(format!(
            ".{name}.{}.tmp",
            TMP.fetch_add(1, Ordering::Relaxed)
        ));
        if std::fs::write(&tmp, &bytes).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return Err(StoreError::Io);
        }
        if std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return Err(StoreError::Io);
        }
        self.used.fetch_add(len, Ordering::Relaxed);
        Ok(format!("{SENTINEL}{name}"))
    }

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

    pub fn sweep(&self, referenced: &HashSet<String>, grace: Duration) -> SweepReport {
        let mut report = SweepReport::default();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return report;
        };
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if referenced.contains(&name) {
                report.kept += 1;
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let young = meta
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_none_or(|age| age < grace);
            if young {
                report.too_young += 1;
                continue;
            }
            let size = meta.len();
            if std::fs::remove_file(entry.path()).is_ok() {
                report.deleted += 1;
                report.freed_bytes += size;
            }
        }
        let _ = self
            .used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |u| {
                Some(u.saturating_sub(report.freed_bytes))
            });
        report
    }

    pub fn existing(&self, stored: &str) -> Option<String> {
        let name = sanitize(stored.strip_prefix(SENTINEL)?)?;
        self.dir
            .join(&name)
            .is_file()
            .then(|| format!("{SENTINEL}{name}"))
    }
}

/// Emoji rows hold the bare name and everything else the `media:` form.
pub fn is_address(stored: &str) -> bool {
    sanitize(stored.strip_prefix(SENTINEL).unwrap_or(stored)).is_some()
}

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

/// The claimed type is checked against the bytes, so what is stored under
/// `.png` decodes as one and nothing else can be parked here under that name.
fn looks_like(ext: &str, bytes: &[u8]) -> bool {
    match ext {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "webp" => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "avif" => {
            bytes.len() >= 12
                && &bytes[4..8] == b"ftyp"
                && matches!(&bytes[8..12], b"avif" | b"avis")
        }
        _ => false,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (MediaStore, tempdir::Dir) {
        let dir = tempdir::Dir::new();
        (
            MediaStore::open(dir.path(), DEFAULT_MAX_BYTES).expect("open"),
            dir,
        )
    }

    mod tempdir {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU32, Ordering};
        pub struct Dir(PathBuf);
        impl Dir {
            pub fn new() -> Dir {
                static N: AtomicU32 = AtomicU32::new(0);
                let p = std::env::temp_dir().join(format!(
                    "dxf-media-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&p).expect("mkdir");
                Dir(p)
            }
            pub fn path(&self) -> PathBuf {
                self.0.clone()
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    const PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

    fn png_bytes() -> u64 {
        let payload = PNG.split_once(";base64,").unwrap().1;
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap()
            .len() as u64
    }

    #[test]
    fn an_unreferenced_blob_is_reclaimed_and_a_referenced_one_is_not() {
        let (media, dir) = store();
        let sentinel = media.store_data_url(PNG).expect("stored");
        let name = sentinel.strip_prefix(SENTINEL).unwrap().to_string();
        assert!(dir.path().join(&name).exists(), "blob was written");

        let referenced: HashSet<String> = [name.clone()].into_iter().collect();
        let report = media.sweep(&referenced, Duration::ZERO);
        assert_eq!(report.kept, 1);
        assert_eq!(report.deleted, 0);
        assert!(dir.path().join(&name).exists());

        let report = media.sweep(&HashSet::new(), Duration::ZERO);
        assert_eq!(report.deleted, 1);
        assert!(report.freed_bytes > 0, "freed bytes must be counted");
        assert!(!dir.path().join(&name).exists());
    }

    #[test]
    fn a_young_orphan_survives_the_grace_period() {
        let (media, dir) = store();
        let sentinel = media.store_data_url(PNG).expect("stored");
        let name = sentinel.strip_prefix(SENTINEL).unwrap().to_string();

        let report = media.sweep(&HashSet::new(), Duration::from_secs(3600));
        assert_eq!(report.deleted, 0, "a blob written seconds ago must survive");
        assert_eq!(report.too_young, 1);
        assert!(dir.path().join(&name).exists());
    }

    #[test]
    fn only_a_content_address_gets_past_sanitize() {
        let ok = format!("{}.png", "a".repeat(64));
        assert_eq!(sanitize(&ok).as_deref(), Some(ok.as_str()));

        for bad in [
            "../../etc/passwd",
            "..%2f..%2fetc%2fpasswd",
            "....//....//etc/passwd",
            &format!("../{}.png", "a".repeat(64)),
            &format!("{}/../../secret.png", "a".repeat(64)),
            "/etc/passwd",
            r"C:\Windows\win.ini",
            r"\\server\share\file.png",
            &format!("{}.png", "g".repeat(64)),
            &format!("{}.png", "a".repeat(63)),
            &format!("{}.png", "a".repeat(65)),
            &format!("{}.", "a".repeat(64)),
            &format!("{}.p n g", "a".repeat(64)),
            &format!("{}.toolong", "a".repeat(64)),
            &"a".repeat(64),
            "",
        ] {
            assert_eq!(sanitize(bad), None, "sanitize accepted {bad:?}");
        }
    }

    #[test]
    fn existing_answers_only_for_a_blob_on_disk() {
        let (media, dir) = store();
        std::fs::write(dir.path().join("secret.txt"), b"not a picture").expect("write");
        let stored = media.store_data_url(PNG).expect("stored");

        assert_eq!(media.existing(&stored).as_deref(), Some(stored.as_str()));
        assert!(media.existing("media:secret.txt").is_none());
        assert!(media.existing("media:../secret.txt").is_none());
        assert!(
            media
                .existing(&format!("media:{}.png", "b".repeat(64)))
                .is_none(),
            "a well-formed address nobody uploaded is not one"
        );
    }

    #[test]
    fn a_temp_file_is_never_swept() {
        let (media, dir) = store();
        let tmp = dir.path().join(".deadbeef.png.tmp");
        std::fs::write(&tmp, b"half a picture").expect("write");

        let report = media.sweep(&HashSet::new(), Duration::ZERO);
        assert_eq!(report.deleted, 0);
        assert!(tmp.exists(), "the temp file must be left for its owner");
    }

    #[test]
    fn the_bytes_have_to_be_what_the_label_says() {
        let (media, _dir) = store();
        let png_payload = PNG.split_once(";base64,").unwrap().1;

        for wrong in ["image/jpeg", "image/gif", "image/webp", "image/avif"] {
            assert_eq!(
                media.store_data_url(&format!("data:{wrong};base64,{png_payload}")),
                Err(StoreError::Unsupported),
                "PNG bytes were accepted as {wrong}"
            );
        }
        let html = base64::engine::general_purpose::STANDARD.encode(b"<html>hi</html>");
        assert_eq!(
            media.store_data_url(&format!("data:image/png;base64,{html}")),
            Err(StoreError::Unsupported)
        );
        assert_eq!(
            media.store_data_url("data:image/svg+xml;base64,PHN2Zz4="),
            Err(StoreError::Unsupported)
        );
        assert_eq!(
            media.store_data_url("data:image/png;base64,!!!not base64"),
            Err(StoreError::Unsupported)
        );
        assert!(media.store_data_url(PNG).is_ok());
    }

    #[test]
    fn the_quota_is_enforced_and_follows_writes_and_sweeps() {
        let dir = tempdir::Dir::new();
        let media = MediaStore::open(dir.path(), png_bytes() + 10).expect("open");
        assert_eq!(media.used_bytes(), 0);

        let stored = media.store_data_url(PNG).expect("first fits");
        assert_eq!(media.used_bytes(), png_bytes());
        assert_eq!(
            media.store_data_url(PNG).as_deref(),
            Ok(stored.as_str()),
            "the same bytes again cost nothing"
        );

        const OTHER: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        assert_eq!(media.store_data_url(OTHER), Err(StoreError::Full));

        let report = media.sweep(&HashSet::new(), Duration::ZERO);
        assert_eq!(report.deleted, 1);
        assert_eq!(media.used_bytes(), 0, "a sweep gives the space back");
        assert!(media.store_data_url(OTHER).is_ok());

        let reopened = MediaStore::open(dir.path(), DEFAULT_MAX_BYTES).expect("reopen");
        assert_eq!(
            reopened.used_bytes(),
            png_bytes(),
            "usage is recounted at open"
        );
    }
}
