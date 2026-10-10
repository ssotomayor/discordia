//! Blossom (BUD-01/02): the picture of a saved emoji goes to the person's
//! blob server once, signed with their key, and a DM then carries the URL
//! instead of the bytes. Content-addressed, so the server's answer is checked
//! against the hash we computed and a re-upload of the same bytes is free.

use std::time::Duration;

use base64::Engine as _;
use secp256k1::SecretKey;
use sha2::{Digest, Sha256};

use super::event::{self, Event};

pub const KIND_AUTH: u16 = 24242;

/// Offered in the picker. The first is the default: free uploads of images up
/// to 20 MiB with no expiry, from any key.
pub const PRESETS: [&str; 3] = [
    "https://blossom.nostr.build",
    "https://blossom.primal.net",
    "https://nostr.download",
];
/// Long enough for a slow upload, short enough that a captured header is
/// worth little.
pub const AUTH_TTL_SECS: i64 = 300;
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(20);

/// `https://host[/path]` without a trailing slash, or nothing: the DM tag
/// builder only sends `https://` urls, so a plain `http://` server would be
/// an upload nobody could use.
pub fn normalize_server(raw: &str) -> Option<String> {
    let s = raw.trim().trim_end_matches('/');
    let host = s.strip_prefix("https://")?;
    (!host.is_empty() && !host.contains(char::is_whitespace)).then(|| s.to_string())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn auth_event(secret: &SecretKey, sha256: &str, now: i64) -> Event {
    event::sign_with(
        secret,
        now,
        KIND_AUTH,
        vec![
            vec!["t".into(), "upload".into()],
            vec!["x".into(), sha256.to_string()],
            vec!["expiration".into(), (now + AUTH_TTL_SECS).to_string()],
        ],
        "Upload an emoji".into(),
    )
}

pub fn auth_header(event: &Event) -> String {
    let json = serde_json::to_vec(event).expect("an event serialises");
    format!(
        "Nostr {}",
        base64::engine::general_purpose::STANDARD.encode(json)
    )
}

/// The url out of a blob descriptor, only when the server stored the bytes
/// we hashed and will serve them where a DM may point.
pub fn blob_url(body: &str, sha256: &str) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "the server did not describe the blob")?;
    let got = v.get("sha256").and_then(|s| s.as_str()).unwrap_or_default();
    if !got.eq_ignore_ascii_case(sha256) {
        return Err("the server stored different bytes than it was sent".into());
    }
    let url = v
        .get("url")
        .and_then(|u| u.as_str())
        .ok_or("the server gave no url")?;
    if !url.starts_with("https://") {
        return Err("the server's url is not https".into());
    }
    Ok(url.to_string())
}

pub fn split_data_url(data_url: &str) -> Option<(String, Vec<u8>)> {
    let (mime, payload) = data_url.strip_prefix("data:")?.split_once(";base64,")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()?;
    (!mime.is_empty() && !bytes.is_empty()).then(|| (mime.to_string(), bytes))
}

pub async fn upload(
    server: &str,
    secret: &SecretKey,
    data_url: &str,
    now: i64,
) -> Result<String, String> {
    let server =
        normalize_server(server).ok_or("the blob server address must start with https://")?;
    let (mime, bytes) = split_data_url(data_url).ok_or("the picture is not a data URL")?;
    let sha = sha256_hex(&bytes);
    let auth = auth_header(&auth_event(secret, &sha, now));
    let http = reqwest::Client::builder()
        .timeout(UPLOAD_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let res = http
        .put(format!("{server}/upload"))
        .header("Authorization", auth)
        .header("Content-Type", mime)
        .body(bytes)
        .send()
        .await
        .map_err(|e| format!("{server} unreachable: {e}"))?;
    let status = res.status();
    // BUD-01 puts the human reason in a header; the body is often empty.
    let reason = res
        .headers()
        .get("x-reason")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        let why = reason.unwrap_or_else(|| body.chars().take(120).collect());
        return Err(format!("{server} answered {status}: {why}"));
    }
    blob_url(&body, &sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_address_is_https_without_a_trailing_slash_or_nothing() {
        assert_eq!(
            normalize_server(" https://blossom.example/ "),
            Some("https://blossom.example".into())
        );
        assert_eq!(normalize_server("http://blossom.example"), None);
        assert_eq!(normalize_server("https://"), None);
        assert_eq!(normalize_server("blossom.example"), None);
    }

    #[test]
    fn the_auth_event_names_the_upload_its_hash_and_an_expiry() {
        let secret = SecretKey::from_slice(&[7; 32]).unwrap();
        let sha = sha256_hex(b"png");
        let ev = auth_event(&secret, &sha, 1_700_000_000);
        assert_eq!(ev.kind, KIND_AUTH);
        assert!(
            ev.tags
                .contains(&vec!["t".to_string(), "upload".to_string()])
        );
        assert!(ev.tags.contains(&vec!["x".to_string(), sha.clone()]));
        assert!(ev.tags.contains(&vec![
            "expiration".to_string(),
            (1_700_000_000 + AUTH_TTL_SECS).to_string()
        ]));
        assert!(auth_header(&ev).starts_with("Nostr "));
    }

    #[test]
    fn a_descriptor_is_accepted_only_for_our_bytes_at_an_https_url() {
        let sha = sha256_hex(b"png");
        let ok = format!(r#"{{"url":"https://b.example/{sha}.png","sha256":"{sha}","size":3}}"#);
        assert_eq!(
            blob_url(&ok, &sha).unwrap(),
            format!("https://b.example/{sha}.png")
        );
        let other = format!(
            r#"{{"url":"https://b.example/x.png","sha256":"{}"}}"#,
            "0".repeat(64)
        );
        assert!(
            blob_url(&other, &sha)
                .unwrap_err()
                .contains("different bytes")
        );
        let plain = format!(r#"{{"url":"http://b.example/{sha}.png","sha256":"{sha}"}}"#);
        assert!(blob_url(&plain, &sha).unwrap_err().contains("https"));
        assert!(blob_url("not json", &sha).is_err());
    }

    #[test]
    fn a_data_url_splits_into_its_type_and_bytes() {
        let (mime, bytes) = split_data_url("data:image/png;base64,cG5n").unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, b"png");
        assert!(split_data_url("https://x/y.png").is_none());
        assert!(split_data_url("data:image/png;base64,").is_none());
    }
}
