use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{net::IpAddr, path::PathBuf, time::Duration};
use zeroize::{Zeroize, Zeroizing};

use super::{blossom, event, nip44};

pub const KIND: u16 = 15;
pub const MAX_BYTES: usize = crate::chat_image::MAX_DM_BYTES;
pub const MAX_CIPHER_BYTES: usize = MAX_BYTES + 16;
pub const MAX_JOBS: usize = 8;
const CACHE_BYTES: u64 = if cfg!(target_os = "android") {
    128 * 1024 * 1024
} else {
    256 * 1024 * 1024
};
const CACHE_TTL: Duration = Duration::from_secs(7 * 86400);

pub fn prepare_permit() -> Result<tokio::sync::OwnedSemaphorePermit, String> {
    static SLOTS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    SLOTS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| "Wait for the current image to finish preparing. Your draft was kept.".into())
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Descriptor {
    pub url: String,
    pub mime: String,
    key: String,
    nonce: String,
    pub hash: String,
    pub original_hash: String,
    pub size: usize,
}

impl std::fmt::Debug for Descriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EncryptedImage(<redacted>)")
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

fn supported_mime(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

fn decode_field(raw: &str, length: usize) -> Result<Zeroizing<Vec<u8>>, String> {
    if raw.len() > 128 {
        return Err("Invalid image key or nonce.".into());
    }
    let bytes = if raw.len() == length * 2 && raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        hex::decode(raw).map_err(|_| "Invalid image key or nonce.")?
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(raw)
            .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(raw))
            .map_err(|_| "Invalid image key or nonce.")?
    };
    if bytes.len() != length {
        return Err("Invalid image key or nonce.".into());
    }
    Ok(Zeroizing::new(bytes))
}

pub fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Descriptor {
    pub fn validate(&self) -> Result<(), String> {
        if !supported_mime(&self.mime)
            || !valid_hash(&self.hash)
            || !valid_hash(&self.original_hash)
            || (self.size != 0 && !(17..=MAX_CIPHER_BYTES).contains(&self.size))
            || hex::decode(&self.key).is_err()
            || self.key.len() != 64
            || self.nonce.len() != 24
            || hex::decode(&self.nonce).is_err()
        {
            return Err("Invalid encrypted image metadata.".into());
        }
        if !self.url.is_empty() {
            public_url(&self.url)?;
        }
        Ok(())
    }

    pub fn rumor(&self, owner: &str, peer: &str, reply: Option<&str>, at: i64) -> event::Rumor {
        let mut tags = vec![vec!["p".into(), peer.into()]];
        for (tag, value) in [
            ("file-type", self.mime.as_str()),
            ("encryption-algorithm", "aes-gcm"),
            ("decryption-key", &self.key),
            ("decryption-nonce", &self.nonce),
            ("x", &self.hash),
            ("ox", &self.original_hash),
            ("size", &self.size.to_string()),
        ] {
            tags.push(vec![tag.into(), value.into()]);
        }
        if let Some(reply) = reply {
            tags.push(vec![
                "e".into(),
                reply.into(),
                String::new(),
                "reply".into(),
            ]);
        }
        event::rumor(owner, at, KIND, tags, self.url.clone())
    }

    pub fn parse(rumor: &event::Rumor) -> Result<Self, String> {
        for name in [
            "file-type",
            "encryption-algorithm",
            "decryption-key",
            "decryption-nonce",
            "x",
            "ox",
            "size",
            "p",
        ] {
            if rumor
                .tags
                .iter()
                .filter(|tag| tag.first().is_some_and(|value| value == name))
                .count()
                > 1
            {
                return Err("Ambiguous encrypted image metadata.".into());
            }
        }
        let get = |tag| {
            rumor
                .tag(tag)
                .ok_or_else(|| "Incomplete encrypted image metadata.".to_string())
        };
        if rumor.kind != KIND || get("encryption-algorithm")? != "aes-gcm" {
            return Err("Unsupported image encryption.".into());
        }
        let descriptor = Self {
            url: rumor.content.clone(),
            mime: get("file-type")?.into(),
            key: hex::encode(decode_field(get("decryption-key")?, 32)?.as_slice()),
            nonce: hex::encode(decode_field(get("decryption-nonce")?, 12)?.as_slice()),
            hash: get("x")?.to_ascii_lowercase(),
            original_hash: get("ox")?.to_ascii_lowercase(),
            size: rumor
                .tag("size")
                .map(str::parse)
                .transpose()
                .map_err(|_| "Invalid image size.")?
                .unwrap_or(0),
        };
        descriptor.validate()?;
        if descriptor.url.is_empty() {
            return Err("Missing encrypted image URL.".into());
        }
        Ok(descriptor)
    }

    pub fn decrypt_raw(&self, ciphertext: &[u8]) -> Result<(Zeroizing<Vec<u8>>, String), String> {
        self.validate()?;
        if ciphertext.len() > MAX_CIPHER_BYTES
            || (self.size != 0 && ciphertext.len() != self.size)
            || blossom::sha256_hex(ciphertext) != self.hash
        {
            return Err("Encrypted image failed its integrity check.".into());
        }
        let key = Zeroizing::new(hex::decode(&self.key).map_err(|_| "Invalid image key.")?);
        let nonce = hex::decode(&self.nonce).map_err(|_| "Invalid image nonce.")?;
        let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(|_| "Invalid image key.")?;
        let plain = Zeroizing::new(
            cipher
                .decrypt(Nonce::from_slice(&nonce), ciphertext)
                .map_err(|_| "Image authentication failed.")?,
        );
        if blossom::sha256_hex(&plain) != self.original_hash {
            return Err("Image hash does not match.".into());
        }
        let image = crate::chat_image::from_bytes_with_limit(&plain, MAX_BYTES)?;
        if !image.starts_with(&format!("data:{};base64,", self.mime)) {
            return Err("Image type does not match.".into());
        }
        Ok((plain, image))
    }

    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<String, String> {
        self.decrypt_raw(ciphertext).map(|(_, image)| image)
    }

    pub fn encrypt_thumb(&self, thumb_bytes: &[u8]) -> Result<Vec<u8>, String> {
        let key = Zeroizing::new(hex::decode(&self.key).map_err(|_| "Invalid image key.")?);
        let mut nonce = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let cipher =
            Aes256Gcm::new_from_slice(key.as_ref()).map_err(|_| "Could not create image key.")?;
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), thumb_bytes)
            .map_err(|_| "Could not encrypt thumbnail.")?;
        let mut result = Vec::with_capacity(12 + ciphertext.len());
        result.extend_from_slice(&nonce);
        result.extend_from_slice(&ciphertext);
        Ok(result)
    }

    pub fn decrypt_thumb(&self, payload: &[u8]) -> Result<String, String> {
        if payload.len() < 12 + 16 {
            return Err("Invalid thumbnail payload.".into());
        }
        let (nonce, ciphertext) = payload.split_at(12);
        let key = Zeroizing::new(hex::decode(&self.key).map_err(|_| "Invalid image key.")?);
        let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(|_| "Invalid image key.")?;
        let plain = Zeroizing::new(
            cipher
                .decrypt(Nonce::from_slice(nonce), ciphertext)
                .map_err(|_| "Thumbnail decryption failed.")?,
        );
        String::from_utf8(plain.to_vec()).map_err(|_| "Invalid thumbnail string.".into())
    }
}

pub fn encrypt(data_url: &str) -> Result<(Descriptor, Vec<u8>), String> {
    if data_url.len() > MAX_BYTES.div_ceil(3) * 4 + 128 {
        return Err("Image too large (max 10 MiB).".into());
    }
    let (mime, bytes) = blossom::split_data_url(data_url).ok_or("Invalid image.")?;
    let bytes = Zeroizing::new(bytes);
    if !supported_mime(&mime) {
        return Err("Unsupported image type.".into());
    }
    crate::chat_image::from_bytes_with_limit(&bytes, MAX_BYTES)?;
    let mut key = Zeroizing::new([0u8; 32]);
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(key.as_mut());
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let cipher =
        Aes256Gcm::new_from_slice(key.as_ref()).map_err(|_| "Could not create image key.")?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), bytes.as_slice())
        .map_err(|_| "Could not encrypt image.")?;
    Ok((
        Descriptor {
            url: String::new(),
            mime,
            key: hex::encode(key.as_ref()),
            nonce: hex::encode(nonce),
            hash: blossom::sha256_hex(&ciphertext),
            original_hash: blossom::sha256_hex(&bytes),
            size: ciphertext.len(),
        },
        ciphertext,
    ))
}

pub fn public_url(raw: &str) -> Result<url::Url, String> {
    if raw.len() > 4096 {
        return Err("File URL is too long.".into());
    }
    let url = url::Url::parse(raw).map_err(|_| "Invalid file server URL.")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.host_str().is_some_and(|h| {
            h.eq_ignore_ascii_case("localhost")
                || h.ends_with(".localhost")
                || h.ends_with(".local")
        })
    {
        return Err("Files require a public HTTPS address.".into());
    }
    if let Some(url::Host::Ipv4(ip)) = url.host()
        && !public_ip(IpAddr::V4(ip))
    {
        return Err("Private file server addresses are not allowed.".into());
    }
    if let Some(url::Host::Ipv6(ip)) = url.host()
        && !public_ip(IpAddr::V6(ip))
    {
        return Err("Private file server addresses are not allowed.".into());
    }
    Ok(url)
}

fn public_ip(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => {
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && ip.octets()[0] != 0
                && ip.octets()[0] < 224
                && !(ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
                && !(ip.octets()[0] == 198 && matches!(ip.octets()[1], 18 | 19))
                && !(ip.octets()[0] == 192 && ip.octets()[1] == 0 && ip.octets()[2] == 0)
                && !(ip.octets()[0] == 192 && ip.octets()[1] == 88 && ip.octets()[2] == 99)
        }
        IpAddr::V6(ip) => {
            !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && ip.segments()[0] & 0xfe00 != 0xfc00
                && ip.segments()[0] & 0xffc0 != 0xfe80
                && ip.segments()[0] & 0xe000 == 0x2000
                && !(ip.segments()[0] == 0x2001
                    && matches!(ip.segments()[1], 0 | 2 | 0x10 | 0x20 | 0x0db8))
                && ip.segments()[0] != 0x2002
        }
    }
}

async fn client(url: &url::Url) -> Result<reqwest::Client, String> {
    let host = url
        .host_str()
        .ok_or("Missing file server.")?
        .trim_matches(['[', ']']);
    let addresses: Vec<_> = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::lookup_host((host, url.port_or_known_default().unwrap_or(443))),
    )
    .await
    .map_err(|_| "File server lookup timed out.")?
    .map_err(|_| "File server lookup failed.")?
    .take(16)
    .collect();
    if addresses.is_empty() || addresses.iter().any(|address| !public_ip(address.ip())) {
        return Err("File server resolved to a private address.".into());
    }
    reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .resolve_to_addrs(host, &addresses)
        .build()
        .map_err(|_| "Could not create file connection.".into())
}

pub async fn upload(
    server: &str,
    secret: &secp256k1::SecretKey,
    bytes: Vec<u8>,
    hash: &str,
) -> Result<String, String> {
    if bytes.len() > MAX_CIPHER_BYTES || blossom::sha256_hex(&bytes) != hash {
        return Err("Invalid encrypted upload.".into());
    }
    let url = public_url(&format!("{}/upload", server.trim_end_matches('/')))?;
    let header = authorization(secret, &url, hash, "upload")?;
    let expected_size = bytes.len();
    let response = client(&url)
        .await?
        .put(url)
        .header("Authorization", header)
        .header("Content-Type", "application/octet-stream")
        .header("X-SHA-256", hash)
        .header("Content-Length", bytes.len())
        .body(bytes)
        .send()
        .await
        .map_err(|_| "Encrypted image upload failed. Retry when connected.")?;
    if !response.status().is_success() {
        return Err(match response.status().as_u16() {
            413 => "The file server does not accept images of this size.",
            415 => "This file server does not accept encrypted images. Choose another server.",
            401 | 403 => "The file server refused access.",
            402 => "This file server requires payment. Choose another server.",
            429 => "The file server is busy. Retry later.",
            _ => "Could not upload the image. Retry later.",
        }
        .into());
    }
    let body = bounded_body(response, 16 * 1024).await?;
    let raw = std::str::from_utf8(&body).map_err(|_| "Invalid upload response.")?;
    let descriptor: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| "Invalid upload response.")?;
    if descriptor.get("size").and_then(|value| value.as_u64()) != Some(expected_size as u64) {
        return Err("The file server reported a different size.".into());
    }
    let url = blossom::blob_url(raw, hash)?;
    public_url(&url)?;
    Ok(url)
}

fn authorization(
    secret: &secp256k1::SecretKey,
    url: &url::Url,
    hash: &str,
    verb: &str,
) -> Result<String, String> {
    let now = chrono::Utc::now().timestamp().saturating_sub(1);
    let event = event::sign_with(
        secret,
        now,
        blossom::KIND_AUTH,
        vec![
            vec!["t".into(), verb.into()],
            vec!["x".into(), hash.into()],
            vec![
                "expiration".into(),
                (now + blossom::AUTH_TTL_SECS).to_string(),
            ],
            vec![
                "server".into(),
                url.host_str()
                    .ok_or("Missing file server.")?
                    .to_ascii_lowercase(),
            ],
        ],
        "Manage encrypted image".into(),
    );
    Ok(format!(
        "Nostr {}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&event).map_err(|_| "Could not authorize image.")?)
    ))
}

async fn bounded_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err("File response too large.".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "File download interrupted.")?
    {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err("File response too large.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn root(owner: &str) -> Result<PathBuf, String> {
    if !valid_hash(owner) {
        return Err("Invalid image account.".into());
    }
    Ok(crate::identity::config_dir().join("dm-images").join(owner))
}

fn cache_root(owner: &str) -> Result<PathBuf, String> {
    if !valid_hash(owner) {
        return Err("Invalid image account.".into());
    }
    let target = crate::identity::cache_dir().join("dm-images").join(owner);
    migrate_legacy_cache(owner, &target);
    Ok(target)
}

fn migrate_legacy_cache(owner: &str, new_root: &std::path::Path) {
    let Ok(legacy_root) = root(owner) else {
        return;
    };
    migrate_legacy_cache_at(&legacy_root, new_root);
}

fn migrate_legacy_cache_at(legacy_root: &std::path::Path, new_root: &std::path::Path) {
    if legacy_root == new_root {
        return;
    }
    let legacy_cache = legacy_root.join("cache");
    if !legacy_cache.is_dir() {
        return;
    }
    let new_cache = new_root.join("cache");
    if !new_cache.exists() {
        if let Some(parent) = new_cache.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::rename(&legacy_cache, &new_cache).is_ok() {
            trim_cache(&new_cache);
            let _ = std::fs::remove_dir(legacy_root);
            return;
        }
    }
    let _ = std::fs::create_dir_all(&new_cache);
    if let Ok(entries) = std::fs::read_dir(&legacy_cache) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Some(name) = path.file_name()
            {
                let dest = new_cache.join(name);
                let should_remove = if dest.exists() {
                    true
                } else {
                    std::fs::rename(&path, &dest).is_ok() || std::fs::copy(&path, &dest).is_ok()
                };
                if should_remove {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&legacy_cache);
    let _ = std::fs::remove_dir(legacy_root);
    trim_cache(&new_cache);
}

pub fn sweep_legacy_caches() {
    let legacy_base = crate::identity::config_dir().join("dm-images");
    let cache_base = crate::identity::cache_dir().join("dm-images");
    sweep_legacy_caches_at(&legacy_base, &cache_base);
}

fn sweep_legacy_caches_at(legacy_base: &std::path::Path, cache_base: &std::path::Path) {
    if legacy_base == cache_base || !legacy_base.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(legacy_base) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir()
            && let Some(owner) = path.file_name().and_then(|n| n.to_str())
            && valid_hash(owner)
        {
            let new_root = cache_base.join(owner);
            migrate_legacy_cache_at(&path, &new_root);
        }
    }
    let _ = std::fs::remove_dir(legacy_base);
}

async fn download_ciphertext(image: &Descriptor) -> Result<Vec<u8>, String> {
    let mut url = public_url(&image.url)?;
    let mut bytes = None;
    for _ in 0..4 {
        let response = client(&url)
            .await?
            .get(url.clone())
            .send()
            .await
            .map_err(|_| "Could not download encrypted image.")?;
        if response.status().is_redirection() {
            let target = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or("Invalid file redirect.")?;
            url = public_url(
                url.join(target)
                    .map_err(|_| "Invalid file redirect.")?
                    .as_str(),
            )?;
            continue;
        }
        if !response.status().is_success() {
            return Err(format!("Image unavailable ({}).", response.status()));
        }
        bytes = Some(
            bounded_body(
                response,
                if image.size == 0 {
                    MAX_CIPHER_BYTES
                } else {
                    image.size
                },
            )
            .await?,
        );
        break;
    }
    bytes.ok_or_else(|| "Too many file redirects.".into())
}

pub async fn receive(owner: &str, image: &Descriptor) -> Result<String, String> {
    static DOWNLOADS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    let _permit = DOWNLOADS
        .acquire()
        .await
        .map_err(|_| "Image downloads unavailable.")?;
    image.validate()?;
    let cache = cache_root(owner)?.join("cache");
    let thumb_path = cache.join(format!("{}.thumb", image.hash));
    if tokio::fs::metadata(&thumb_path).await.is_ok_and(|m| {
        m.is_file()
            && m.modified()
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age <= CACHE_TTL)
    }) && let Ok(bytes) = tokio::fs::read(&thumb_path).await
    {
        let metadata = image.clone();
        if let Ok(Ok(data)) =
            tokio::task::spawn_blocking(move || metadata.decrypt_thumb(&bytes)).await
        {
            return Ok(data);
        }
    }
    let path = cache.join(format!("{}.bin", image.hash));
    let bytes = if tokio::fs::metadata(&path).await.is_ok_and(|m| {
        m.len() <= MAX_CIPHER_BYTES as u64
            && (image.size == 0 || m.len() == image.size as u64)
            && m.modified()
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age <= CACHE_TTL)
    }) && let Ok(bytes) = tokio::fs::read(&path).await
    {
        bytes
    } else {
        let downloaded = download_ciphertext(image).await?;
        if tokio::fs::create_dir_all(&cache).await.is_ok() {
            let cache_ref = cache.clone();
            let path_ref = path.clone();
            let dl_bytes = downloaded.clone();
            let _ =
                tokio::task::spawn_blocking(move || cache_store(&cache_ref, &path_ref, &dl_bytes))
                    .await;
        }
        downloaded
    };
    let metadata = image.clone();
    let (thumb_url, thumb_enc) = tokio::task::spawn_blocking(move || {
        let (plain, _orig) = metadata.decrypt_raw(&bytes)?;
        let thumb_url = crate::chat_image::thumbnail_data_url(&plain)?;
        let thumb_enc = metadata.encrypt_thumb(thumb_url.as_bytes())?;
        Ok::<_, String>((thumb_url, thumb_enc))
    })
    .await
    .map_err(|_| "Could not decode image.")??;

    if tokio::fs::create_dir_all(&cache).await.is_ok() {
        let cache_ref = cache.clone();
        let thumb_ref = thumb_path.clone();
        let _ =
            tokio::task::spawn_blocking(move || cache_store(&cache_ref, &thumb_ref, &thumb_enc))
                .await;
    }
    Ok(thumb_url)
}

pub async fn load_original(owner: &str, image: &Descriptor) -> Result<String, String> {
    static DOWNLOADS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    let _permit = DOWNLOADS
        .acquire()
        .await
        .map_err(|_| "Image downloads unavailable.")?;
    image.validate()?;
    let cache = cache_root(owner)?.join("cache");
    let path = cache.join(format!("{}.bin", image.hash));
    let bytes = if tokio::fs::metadata(&path).await.is_ok_and(|m| {
        m.len() <= MAX_CIPHER_BYTES as u64
            && (image.size == 0 || m.len() == image.size as u64)
            && m.modified()
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age <= CACHE_TTL)
    }) && let Ok(bytes) = tokio::fs::read(&path).await
    {
        bytes
    } else {
        let downloaded = download_ciphertext(image).await?;
        if tokio::fs::create_dir_all(&cache).await.is_ok() {
            let cache_ref = cache.clone();
            let path_ref = path.clone();
            let dl_bytes = downloaded.clone();
            let _ =
                tokio::task::spawn_blocking(move || cache_store(&cache_ref, &path_ref, &dl_bytes))
                    .await;
        }
        downloaded
    };
    let metadata = image.clone();
    tokio::task::spawn_blocking(move || metadata.decrypt(&bytes))
        .await
        .map_err(|_| "Could not decode image.")?
}

fn cache_store(cache: &std::path::Path, path: &std::path::Path, bytes: &[u8]) {
    static WRITE: parking_lot::Mutex<()> = parking_lot::const_mutex(());
    let _write = WRITE.lock();
    trim_cache(cache);
    let temporary = cache.join(format!("{}.tmp", uuid::Uuid::new_v4()));
    if std::fs::write(&temporary, bytes).is_ok() {
        let _ = std::fs::rename(&temporary, path);
    }
    let _ = std::fs::remove_file(temporary);
}

fn trim_cache(root: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata
                .is_file()
                .then_some((metadata.modified().ok(), metadata.len(), entry.path()))
        })
        .collect();
    files.sort_by_key(|entry| entry.0);
    files.retain(|(modified, _, path)| {
        if modified
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age > CACHE_TTL)
        {
            return std::fs::remove_file(path).is_err();
        }
        true
    });
    let mut size: u64 = files.iter().map(|entry| entry.1).sum();
    let mut count = files.len();
    for (_, bytes, path) in files {
        if size <= CACHE_BYTES.saturating_sub(MAX_CIPHER_BYTES as u64) && count < 256 {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            size = size.saturating_sub(bytes);
            count = count.saturating_sub(1);
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub peer: String,
    pub text: String,
    pub reply_to: Option<String>,
    pub server: String,
    pub created_at: i64,
    pub image: Descriptor,
}

impl Job {
    fn path(&self, owner: &str) -> Result<PathBuf, String> {
        if uuid::Uuid::parse_str(&self.id).is_err() {
            return Err("Invalid image job.".into());
        }
        Ok(root(owner)?
            .join("pending")
            .join(format!("{}.json", self.id)))
    }

    pub fn save(&self, secret: &secp256k1::SecretKey) -> Result<(), String> {
        let owner = event::xonly_hex(secret);
        let path = self.path(&owner)?;
        self.save_at(&path, secret)
    }

    fn save_at(&self, path: &std::path::Path, secret: &secp256k1::SecretKey) -> Result<(), String> {
        if path.with_extension("cancel").exists() {
            return Err("This image was canceled.".into());
        }
        crate::identity::write_private(path, &self.seal(secret)?)
    }

    fn seal(&self, secret: &secp256k1::SecretKey) -> Result<String, String> {
        let owner = event::xonly_hex(secret);
        let key = Zeroizing::new(nip44::conversation_key(secret, &owner)?);
        let plain =
            Zeroizing::new(serde_json::to_string(self).map_err(|_| "Could not save image job.")?);
        nip44::encrypt(&key, &plain)
    }

    pub fn remove(&self, owner: &str) {
        if let Ok(path) = self.path(owner) {
            // An upload finishing concurrently must never resurrect a canceled pending job.
            let _ = std::fs::write(path.with_extension("cancel"), b"");
            let _ = std::fs::remove_file(path.with_extension("bin"));
            let _ = std::fs::remove_file(path);
        }
    }

    pub async fn upload(&self, secret: &secp256k1::SecretKey) -> Result<Self, String> {
        if !self.image.url.is_empty() {
            return Ok(self.clone());
        }
        let path = self.path(&event::xonly_hex(secret))?.with_extension("bin");
        if tokio::fs::metadata(&path)
            .await
            .map_err(|_| "Pending image is missing.")?
            .len()
            != self.image.size as u64
        {
            return Err("Pending image size does not match.".into());
        }
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|_| "Could not read pending image.")?;
        let mut job = self.clone();
        job.image.url = upload(&self.server, secret, bytes, &self.image.hash).await?;
        job.save(secret)?;
        Ok(job)
    }
}

pub fn prepare(
    secret: &secp256k1::SecretKey,
    peer: String,
    server: String,
    data: &str,
    text: String,
    reply_to: Option<String>,
) -> Result<Job, String> {
    if !valid_hash(&peer) || text.len() > 8000 {
        return Err("Invalid image recipient or caption.".into());
    }
    secp256k1::XOnlyPublicKey::from_slice(
        &hex::decode(&peer).map_err(|_| "Invalid image recipient.")?,
    )
    .map_err(|_| "Invalid image recipient.")?;
    public_url(&server)?;
    let (image, bytes) = encrypt(data)?;
    let job = Job {
        id: uuid::Uuid::new_v4().to_string(),
        peer,
        text,
        reply_to,
        server,
        created_at: chrono::Utc::now().timestamp(),
        image,
    };
    let owner = event::xonly_hex(secret);
    let path = job.path(&owner)?;
    std::fs::create_dir_all(path.parent().ok_or("Invalid image path.")?)
        .map_err(|_| "Could not create image queue.")?;
    let bin = path.with_extension("bin");
    std::fs::write(&bin, &bytes).map_err(|_| "Could not save encrypted image.")?;
    if let Err(error) = job.save(secret) {
        let _ = std::fs::remove_file(bin);
        return Err(error);
    }
    if let Ok(cache) = cache_root(&owner).map(|r| r.join("cache"))
        && std::fs::create_dir_all(&cache).is_ok()
    {
        let cache_path = cache.join(format!("{}.bin", job.image.hash));
        cache_store(&cache, &cache_path, &bytes);
        if let Some((_mime, raw)) = blossom::split_data_url(data)
            && let Ok(thumb_url) = crate::chat_image::thumbnail_data_url(&raw)
            && let Ok(thumb_enc) = job.image.encrypt_thumb(thumb_url.as_bytes())
        {
            let thumb_path = cache.join(format!("{}.thumb", job.image.hash));
            cache_store(&cache, &thumb_path, &thumb_enc);
        }
    }
    Ok(job)
}

pub fn load_jobs(secret: &secp256k1::SecretKey) -> Vec<Job> {
    sweep_legacy_caches();
    let owner = event::xonly_hex(secret);
    let Ok(root) = root(&owner) else {
        return Vec::new();
    };
    load_jobs_from(&root.join("pending"), secret)
}

fn load_jobs_from(directory: &std::path::Path, secret: &secp256k1::SecretKey) -> Vec<Job> {
    let owner = event::xonly_hex(secret);
    sweep_pending(directory);
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let Ok(key) = nip44::conversation_key(secret, &owner) else {
        return Vec::new();
    };
    let key = Zeroizing::new(key);
    let mut jobs = Vec::new();
    for entry in entries
        .flatten()
        .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
        .take(MAX_JOBS * 4)
    {
        if entry.path().with_extension("cancel").exists() {
            continue;
        }
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json")
            || !entry.metadata().is_ok_and(|m| m.len() <= 64 * 1024)
        {
            continue;
        }
        let Some(job) = std::fs::read_to_string(entry.path())
            .ok()
            .and_then(|sealed| nip44::decrypt(&key, &sealed).ok())
            .and_then(|plain| serde_json::from_str::<Job>(&Zeroizing::new(plain)).ok())
        else {
            continue;
        };
        if job.image.validate().is_ok()
            && valid_hash(&job.peer)
            && uuid::Uuid::parse_str(&job.id).is_ok()
            && directory.join(format!("{}.json", job.id)) == entry.path()
        {
            jobs.push(job);
        }
        if jobs.len() == MAX_JOBS {
            break;
        }
    }
    jobs.sort_by_key(|job| (job.created_at, job.id.clone()));
    jobs
}

fn sweep_pending(directory: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_stem()
            .and_then(|name| name.to_str())
            .is_none_or(|name| uuid::Uuid::parse_str(name).is_err())
        {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|at| at.elapsed().ok());
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("cancel") if old.is_some_and(|age| age > Duration::from_secs(86400)) => {
                let _ = std::fs::remove_file(path.with_extension("json"));
                let _ = std::fs::remove_file(path.with_extension("bin"));
                let _ = std::fs::remove_file(path);
            }
            Some("bin")
                if !path.with_extension("json").exists()
                    && old.is_some_and(|age| age > Duration::from_secs(3600)) =>
            {
                let _ = std::fs::remove_file(path);
            }
            _ => {}
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Waiting,
    Loading(uuid::Uuid),
    Uploading,
    Ready,
    Evicted,
    Failed(String),
}

#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    pub descriptor: Descriptor,
    pub status: Status,
    pub upload: bool,
    pub viewers: std::collections::HashSet<uuid::Uuid>,
    pub data: Option<std::sync::Arc<String>>,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrivateImage")
            .field("status", &self.status)
            .field("upload", &self.upload)
            .field("cached", &self.data.is_some())
            .finish()
    }
}

#[derive(Default)]
pub struct Queue {
    pub jobs: std::collections::HashMap<String, Job>,
    pub failed: std::collections::HashSet<String>,
    tasks: std::collections::HashMap<String, tokio::task::JoinHandle<()>>,
}

impl Drop for Queue {
    fn drop(&mut self) {
        for (_, task) in self.tasks.drain() {
            task.abort();
        }
    }
}

impl Queue {
    pub fn key(&self, id: crate::protocol::Id) -> Option<String> {
        self.jobs
            .keys()
            .find(|key| super::service::message_id(key) == id)
            .cloned()
    }

    pub fn remove(&mut self, key: &str, owner: &str) -> Option<Job> {
        if let Some(task) = self.tasks.remove(key) {
            task.abort();
        }
        self.failed.remove(key);
        let job = self.jobs.remove(key)?;
        job.remove(owner);
        Some(job)
    }

    pub fn complete(&mut self, key: &str) {
        self.tasks.remove(key);
    }

    pub fn pump(
        &mut self,
        secret: secp256k1::SecretKey,
        tx: &tokio::sync::mpsc::UnboundedSender<(String, Result<Job, String>)>,
    ) {
        let mut jobs: Vec<_> = self.jobs.values().cloned().collect();
        jobs.sort_by_key(|job| (job.created_at, job.id.clone()));
        let mut peers: std::collections::HashSet<_> = self
            .tasks
            .keys()
            .filter_map(|key| self.jobs.get(key).map(|job| job.peer.clone()))
            .collect();
        for job in jobs {
            if self.tasks.len() >= 2 {
                break;
            }
            if self.failed.contains(&job.id)
                || self.tasks.contains_key(&job.id)
                || !peers.insert(job.peer.clone())
            {
                continue;
            }
            let key = job.id.clone();
            let result = tx.clone();
            self.tasks.insert(
                key.clone(),
                tokio::spawn(async move {
                    let uploaded = job.upload(&secret).await;
                    let _ = result.send((key, uploaded));
                }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

    #[test]
    fn authenticated_roundtrip_and_tampering() {
        let (image, bytes) = encrypt(PNG).unwrap();
        assert_eq!(image.decrypt(&bytes).unwrap(), PNG);
        let (other, other_bytes) = encrypt(PNG).unwrap();
        assert_ne!(image.key, other.key);
        assert_ne!(bytes, other_bytes);
        assert!(other.decrypt(&bytes).is_err());
        let mut broken = bytes.clone();
        broken[0] ^= 1;
        assert!(image.decrypt(&broken).is_err());
        let mut wrong_key = image.clone();
        wrong_key.key = other.key.clone();
        assert!(wrong_key.decrypt(&bytes).is_err());
        assert!(!format!("{image:?}").contains(&image.key));
    }

    #[test]
    fn file_rumor_is_private_to_sender_and_recipient_and_recoverable() {
        let sender = secp256k1::SecretKey::from_slice(&[3; 32]).unwrap();
        let receiver = secp256k1::SecretKey::from_slice(&[4; 32]).unwrap();
        let stranger = secp256k1::SecretKey::from_slice(&[5; 32]).unwrap();
        let (owner, peer) = (event::xonly_hex(&sender), event::xonly_hex(&receiver));
        let (mut image, bytes) = encrypt(PNG).unwrap();
        image.url = format!("https://example.com/{}.bin", image.hash);
        let rumor = image.rumor(&owner, &peer, Some(&"a".repeat(64)), 100);
        let (theirs, ours) = super::super::nip17::wrap_both(&sender, &peer, &rumor, 100).unwrap();
        for wrap in [&theirs, &ours] {
            let raw = serde_json::to_string(wrap).unwrap();
            assert!(!raw.contains(&image.url));
            assert!(!raw.contains(&image.key));
            assert!(super::super::nip59::unwrap(&stranger, wrap).is_err());
        }
        for (secret, wrap, account) in [(&sender, &ours, &owner), (&receiver, &theirs, &peer)] {
            let opened = super::super::nip59::unwrap(secret, wrap).unwrap();
            let message = super::super::nip17::open_rumor(account, &opened).unwrap();
            assert_eq!(message.author, owner);
            assert_eq!(message.reply_to.as_deref(), Some("a".repeat(64).as_str()));
            assert_eq!(
                Descriptor::parse(&opened).unwrap().decrypt(&bytes).unwrap(),
                PNG
            );
        }
        let mut no_size = rumor.clone();
        no_size.tags.retain(|tag| tag[0] != "size");
        assert_eq!(
            Descriptor::parse(&no_size)
                .unwrap()
                .decrypt(&bytes)
                .unwrap(),
            PNG
        );
        let mut ambiguous = rumor;
        ambiguous
            .tags
            .push(vec!["decryption-key".into(), "0".repeat(64)]);
        assert!(Descriptor::parse(&ambiguous).is_err());
    }

    #[test]
    fn pending_metadata_hides_keys_and_recovers_only_for_its_account() {
        let secret = secp256k1::SecretKey::from_slice(&[3; 32]).unwrap();
        let (image, bytes) = encrypt(PNG).unwrap();
        let job = Job {
            id: uuid::Uuid::new_v4().to_string(),
            peer: event::xonly_hex(&secret),
            text: "private caption".into(),
            reply_to: None,
            server: "https://example.com".into(),
            created_at: 100,
            image,
        };
        let sealed = job.seal(&secret).unwrap();
        assert!(!sealed.contains(&job.image.key));
        assert!(!sealed.contains("private caption"));
        let key = nip44::conversation_key(&secret, &event::xonly_hex(&secret)).unwrap();
        let opened = Zeroizing::new(nip44::decrypt(&key, &sealed).unwrap());
        let restored: Job = serde_json::from_str(&opened).unwrap();
        assert_eq!(restored.image, job.image);
        assert_eq!(restored.id, job.id);
        assert_eq!(restored.image.decrypt(&bytes).unwrap(), PNG);
        let other = secp256k1::SecretKey::from_slice(&[4; 32]).unwrap();
        let other_key = nip44::conversation_key(&other, &event::xonly_hex(&other)).unwrap();
        assert!(nip44::decrypt(&other_key, &sealed).is_err());
    }

    #[test]
    fn pending_disk_jobs_survive_restart_and_cancellation_wins_a_late_save() {
        let directory =
            std::env::temp_dir().join(format!("dm-image-jobs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let secret = secp256k1::SecretKey::from_slice(&[3; 32]).unwrap();
        let other = secp256k1::SecretKey::from_slice(&[4; 32]).unwrap();
        let (image, bytes) = encrypt(PNG).unwrap();
        let job = Job {
            id: uuid::Uuid::new_v4().to_string(),
            peer: event::xonly_hex(&other),
            text: "caption".into(),
            reply_to: None,
            server: "https://nostr.download".into(),
            created_at: 100,
            image,
        };
        let path = directory.join(format!("{}.json", job.id));
        std::fs::write(path.with_extension("bin"), &bytes).unwrap();
        job.save_at(&path, &secret).unwrap();
        let restored = load_jobs_from(&directory, &secret);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].id, job.id);
        assert_eq!(
            restored[0]
                .image
                .decrypt(&std::fs::read(path.with_extension("bin")).unwrap())
                .unwrap(),
            PNG
        );
        assert!(load_jobs_from(&directory, &other).is_empty());
        let sealed = std::fs::read_to_string(&path).unwrap();
        assert!(!sealed.contains(&job.image.key));
        std::fs::write(path.with_extension("cancel"), b"").unwrap();
        assert!(job.save_at(&path, &secret).is_err());
        std::fs::write(&path, sealed).unwrap();
        assert!(load_jobs_from(&directory, &secret).is_empty());
        for extension in ["json", "bin", "cancel"] {
            std::fs::remove_file(path.with_extension(extension)).unwrap();
        }
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn prepare_saves_job_and_populates_local_cache() {
        let secret = secp256k1::SecretKey::from_slice(&[5; 32]).unwrap();
        let peer = secp256k1::SecretKey::from_slice(&[6; 32]).unwrap();
        let peer_hex = event::xonly_hex(&peer);
        let owner = event::xonly_hex(&secret);
        let job = prepare(
            &secret,
            peer_hex,
            "https://nostr.download".into(),
            PNG,
            "caption".into(),
            None,
        )
        .unwrap();
        let cache_file = cache_root(&owner)
            .unwrap()
            .join("cache")
            .join(format!("{}.bin", job.image.hash));
        assert!(cache_file.exists());
        let cached_bytes = std::fs::read(&cache_file).unwrap();
        assert_eq!(job.image.decrypt(&cached_bytes).unwrap(), PNG);

        let thumb_file = cache_root(&owner)
            .unwrap()
            .join("cache")
            .join(format!("{}.thumb", job.image.hash));
        assert!(thumb_file.exists());
        let thumb_bytes = std::fs::read(&thumb_file).unwrap();
        let thumb_url = job.image.decrypt_thumb(&thumb_bytes).unwrap();
        assert!(thumb_url.starts_with("data:image/"));

        job.remove(&owner);
        let _ = std::fs::remove_file(job.path(&owner).unwrap().with_extension("cancel"));
        let _ = std::fs::remove_file(&cache_file);
        let _ = std::fs::remove_file(&thumb_file);
    }

    #[test]
    fn image_cache_is_encrypted_bounded_and_expires_without_deleting_messages() {
        let directory =
            std::env::temp_dir().join(format!("dm-image-cache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let (image, bytes) = encrypt(PNG).unwrap();
        let path = directory.join(format!("{}.bin", image.hash));
        cache_store(&directory, &path, &bytes);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        for index in 0..300 {
            std::fs::write(directory.join(format!("{index}.bin")), b"ciphertext").unwrap();
        }
        trim_cache(&directory);
        assert!(std::fs::read_dir(&directory).unwrap().count() < 256);
        let expired = directory.join("expired.bin");
        std::fs::write(&expired, b"ciphertext").unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&expired)
            .unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
            .unwrap();
        drop(file);
        trim_cache(&directory);
        assert!(!expired.exists());
        for entry in std::fs::read_dir(&directory).unwrap().flatten() {
            std::fs::remove_file(entry.path()).unwrap();
        }
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn image_limit_is_exactly_ten_mib_and_community_default_stays_two() {
        let (_, mut bytes) = blossom::split_data_url(PNG).unwrap();
        bytes.resize(MAX_BYTES, 0);
        let url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        let (image, encrypted) = encrypt(&url).unwrap();
        assert_eq!(image.size, MAX_BYTES + 16);
        assert_eq!(image.decrypt(&encrypted).unwrap(), url);
        bytes.push(0);
        let excessive = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        assert!(encrypt(&excessive).is_err());
        assert_eq!(crate::chat_image::MAX_BYTES, 2 * 1024 * 1024);
    }

    #[test]
    fn authorization_is_scoped_to_server_ciphertext_and_action() {
        let secret = secp256k1::SecretKey::from_slice(&[3; 32]).unwrap();
        let hash = "a".repeat(64);
        let header = authorization(
            &secret,
            &public_url("https://example.com/upload").unwrap(),
            &hash,
            "upload",
        )
        .unwrap();
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(header.strip_prefix("Nostr ").unwrap())
            .unwrap();
        let event: event::Event = serde_json::from_slice(&raw).unwrap();
        assert!(event.verify());
        assert_eq!(event.kind, 24242);
        assert_eq!(event.tag("server"), Some("example.com"));
        assert_eq!(event.tag("x"), Some(hash.as_str()));
        assert_eq!(event.tag("t"), Some("upload"));
        assert!(event.tag("expiration").is_some());
    }

    #[tokio::test]
    #[ignore = "uploads a generated encrypted test image to an explicitly selected Blossom server"]
    async fn encrypted_binary_upload_probe() {
        let server = std::env::var("DISCORDIA_TEST_BLOSSOM_SERVER").expect("choose a server");
        let secret = secp256k1::SecretKey::new(&mut rand::rngs::OsRng);
        let (mut image, bytes) = encrypt(PNG).unwrap();
        image.url = upload(&server, &secret, bytes.clone(), &image.hash)
            .await
            .unwrap();
        let url = public_url(&image.url).unwrap();
        let response = client(&url).await.unwrap().get(url).send().await.unwrap();
        let downloaded = bounded_body(response, MAX_CIPHER_BYTES).await.unwrap();
        let management =
            public_url(&format!("{}/{}", server.trim_end_matches('/'), image.hash)).unwrap();
        let authorization = authorization(&secret, &management, &image.hash, "delete").unwrap();
        let deleted = client(&management)
            .await
            .unwrap()
            .delete(management)
            .header("Authorization", authorization)
            .send()
            .await
            .unwrap();
        assert!(
            deleted.status().is_success(),
            "test blob cleanup failed: {}",
            deleted.status()
        );
        assert_eq!(downloaded, bytes);
        assert_eq!(image.decrypt(&downloaded).unwrap(), PNG);
    }

    #[test]
    fn aes256_gcm_matches_independent_nist_vector() {
        let cipher = Aes256Gcm::new_from_slice(&[0; 32]).unwrap();
        let encrypted = cipher
            .encrypt(Nonce::from_slice(&[0; 12]), [0u8; 16].as_slice())
            .unwrap();
        assert_eq!(
            hex::encode(encrypted),
            "cea7403d4d606b6e074ec5d3baf39d18d0d1c8a799996bf0265b98b5d48ab919"
        );
    }

    #[test]
    fn untrusted_urls_cannot_target_local_networks() {
        for url in [
            "http://example.com/x",
            "https://127.0.0.1/x",
            "https://10.0.0.2/x",
            "https://[::ffff:127.0.0.1]/x",
            "https://[fe80::1]/x",
            "https://localhost/x",
            "https://a.local/x",
            "https://me:secret@example.com/x",
        ] {
            assert!(public_url(url).is_err(), "{url}");
        }
        assert!(public_url("https://example.com/image.bin").is_ok());
    }

    #[tokio::test]
    async fn queue_failed_job_does_not_block_later_jobs_to_same_peer() {
        let secret = secp256k1::SecretKey::from_slice(&[3; 32]).unwrap();
        let peer_a = "a".repeat(64);
        let peer_b = "b".repeat(64);
        let (image, _) = encrypt(PNG).unwrap();
        let job1 = Job {
            id: "job1".into(),
            peer: peer_a.clone(),
            text: "first".into(),
            reply_to: None,
            server: "https://example.com".into(),
            created_at: 100,
            image: image.clone(),
        };
        let job2 = Job {
            id: "job2".into(),
            peer: peer_a.clone(),
            text: "second".into(),
            reply_to: None,
            server: "https://example.com".into(),
            created_at: 200,
            image: image.clone(),
        };
        let job3 = Job {
            id: "job3".into(),
            peer: peer_b.clone(),
            text: "other peer".into(),
            reply_to: None,
            server: "https://example.com".into(),
            created_at: 300,
            image,
        };

        let mut queue = Queue::default();
        queue.jobs.insert(job1.id.clone(), job1);
        queue.jobs.insert(job2.id.clone(), job2);
        queue.jobs.insert(job3.id.clone(), job3);
        queue.failed.insert("job1".into());

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        queue.pump(secret, &tx);

        assert!(
            !queue.tasks.contains_key("job1"),
            "failed job must not spawn"
        );
        assert!(
            queue.tasks.contains_key("job2"),
            "later job to same peer must spawn when earlier job failed"
        );
        assert!(
            queue.tasks.contains_key("job3"),
            "job to different peer must also spawn"
        );
    }

    #[tokio::test]
    async fn queue_active_job_blocks_concurrent_job_to_same_peer() {
        let secret = secp256k1::SecretKey::from_slice(&[3; 32]).unwrap();
        let peer_a = "a".repeat(64);
        let (image, _) = encrypt(PNG).unwrap();
        let job1 = Job {
            id: "job1".into(),
            peer: peer_a.clone(),
            text: "first".into(),
            reply_to: None,
            server: "https://example.com".into(),
            created_at: 100,
            image: image.clone(),
        };
        let job2 = Job {
            id: "job2".into(),
            peer: peer_a.clone(),
            text: "second".into(),
            reply_to: None,
            server: "https://example.com".into(),
            created_at: 200,
            image,
        };

        let mut queue = Queue::default();
        queue.jobs.insert(job1.id.clone(), job1);
        queue.jobs.insert(job2.id.clone(), job2);

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        queue.pump(secret, &tx);

        assert!(queue.tasks.contains_key("job1"), "first job should start");
        assert!(
            !queue.tasks.contains_key("job2"),
            "second job to same peer must wait"
        );
    }

    #[test]
    fn legacy_cache_migrates_to_cache_dir_and_cleans_up_orphaned_directory() {
        let dir = std::env::temp_dir().join(format!("dm-image-migration-{}", uuid::Uuid::new_v4()));
        let legacy_root = dir.join("config").join("dm-images").join("owner");
        let new_root = dir.join("cache").join("dm-images").join("owner");

        let legacy_cache = legacy_root.join("cache");
        std::fs::create_dir_all(&legacy_cache).unwrap();
        std::fs::write(legacy_cache.join("image.bin"), b"encrypted-image-data").unwrap();
        std::fs::write(legacy_cache.join("image.thumb"), b"thumb-data").unwrap();

        let legacy_pending = legacy_root.join("pending");
        std::fs::create_dir_all(&legacy_pending).unwrap();
        std::fs::write(legacy_pending.join("job.json"), b"{}").unwrap();

        migrate_legacy_cache_at(&legacy_root, &new_root);

        let new_cache = new_root.join("cache");
        assert_eq!(
            std::fs::read(new_cache.join("image.bin")).unwrap(),
            b"encrypted-image-data"
        );
        assert_eq!(
            std::fs::read(new_cache.join("image.thumb")).unwrap(),
            b"thumb-data"
        );
        assert!(!legacy_cache.exists());
        assert!(legacy_pending.join("job.json").exists());
        assert!(legacy_root.exists());

        std::fs::remove_dir_all(&legacy_pending).unwrap();
        std::fs::create_dir_all(&legacy_cache).unwrap();
        std::fs::write(legacy_cache.join("image2.bin"), b"second-image").unwrap();

        migrate_legacy_cache_at(&legacy_root, &new_root);

        assert_eq!(
            std::fs::read(new_cache.join("image2.bin")).unwrap(),
            b"second-image"
        );
        assert_eq!(
            std::fs::read(new_cache.join("image.bin")).unwrap(),
            b"encrypted-image-data"
        );
        assert!(!legacy_root.exists());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sweep_legacy_caches_migrates_all_valid_accounts() {
        let dir = std::env::temp_dir().join(format!("dm-image-sweep-{}", uuid::Uuid::new_v4()));
        let legacy_base = dir.join("config").join("dm-images");
        let cache_base = dir.join("cache").join("dm-images");

        let owner1 = "1".repeat(64);
        let owner2 = "2".repeat(64);
        let invalid = "not-a-valid-hash";

        std::fs::create_dir_all(legacy_base.join(&owner1).join("cache")).unwrap();
        std::fs::write(
            legacy_base.join(&owner1).join("cache").join("1.bin"),
            b"data1",
        )
        .unwrap();

        std::fs::create_dir_all(legacy_base.join(&owner2).join("cache")).unwrap();
        std::fs::write(
            legacy_base.join(&owner2).join("cache").join("2.bin"),
            b"data2",
        )
        .unwrap();

        std::fs::create_dir_all(legacy_base.join(invalid).join("cache")).unwrap();
        std::fs::write(
            legacy_base.join(invalid).join("cache").join("3.bin"),
            b"data3",
        )
        .unwrap();

        sweep_legacy_caches_at(&legacy_base, &cache_base);

        assert_eq!(
            std::fs::read(cache_base.join(&owner1).join("cache").join("1.bin")).unwrap(),
            b"data1"
        );
        assert_eq!(
            std::fs::read(cache_base.join(&owner2).join("cache").join("2.bin")).unwrap(),
            b"data2"
        );
        assert!(!legacy_base.join(&owner1).exists());
        assert!(!legacy_base.join(&owner2).exists());
        assert!(legacy_base.join(invalid).exists());

        std::fs::remove_dir_all(dir).unwrap();
    }
}
