use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use sha2::{Digest, Sha256};

const SENTINEL: &str = "media:";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepReport {
    pub kept: usize,
    pub too_young: usize,
    pub deleted: usize,
    pub freed_bytes: u64,
}

#[derive(Clone)]
pub struct MediaStore {
    dir: PathBuf,
}

impl MediaStore {
    pub fn open(dir: PathBuf) -> std::io::Result<MediaStore> {
        std::fs::create_dir_all(&dir)?;
        Ok(MediaStore { dir })
    }

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
        report
    }

    pub fn read(&self, name: &str) -> Option<(Vec<u8>, &'static str)> {
        let name = sanitize(name)?;
        let bytes = std::fs::read(self.dir.join(&name)).ok()?;
        Some((bytes, mime_for_name(&name)))
    }
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
        ENCRYPTED_BLOB_MIME => Some("enc"),
        _ => None,
    }
}

pub const ENCRYPTED_BLOB_MIME: &str = "application/vnd.discordia.enc";

fn mime_for_name(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, e)| e) {
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("enc") => ENCRYPTED_BLOB_MIME,
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (MediaStore, tempdir::Dir) {
        let dir = tempdir::Dir::new();
        (MediaStore::open(dir.path()).expect("open"), dir)
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
    fn read_refuses_a_path_that_is_not_a_content_address() {
        let (media, dir) = store();
        std::fs::write(dir.path().join("secret.txt"), b"not a picture").expect("write");

        assert!(media.read("secret.txt").is_none());
        assert!(media.read("../secret.txt").is_none());
        assert!(
            media
                .read(&format!("{}/../secret.txt", "a".repeat(64)))
                .is_none()
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
}
