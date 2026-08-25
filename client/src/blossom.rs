use base64::Engine as _;
use sha2::{Digest, Sha256};

use crate::identity::Identity;

const KIND_BLOSSOM_AUTH: i64 = 24242;
const AUTH_TTL_SECS: i64 = 3600;

pub async fn upload_blob(
    server: &str,
    bytes: Vec<u8>,
    mime: &str,
    identity: &Identity,
) -> Result<String, String> {
    let sha_hex = hex::encode(Sha256::digest(&bytes));
    let created_at = chrono::Utc::now().timestamp();
    let expiration = (created_at + AUTH_TTL_SECS).to_string();
    let content = "Upload blob";

    let tags = serde_json::json!([["t", "upload"], ["x", sha_hex], ["expiration", expiration],]);

    let serialized = serde_json::to_string(&serde_json::json!([
        0,
        identity.pubkey,
        created_at,
        KIND_BLOSSOM_AUTH,
        tags.clone(),
        content,
    ]))
    .map_err(|e| e.to_string())?;
    let id_bytes: [u8; 32] = Sha256::digest(serialized.as_bytes()).into();
    let id_hex = hex::encode(id_bytes);
    let sig = identity.nostr_sign_id(&id_bytes);

    let event = serde_json::json!({
        "id": id_hex,
        "pubkey": identity.pubkey,
        "created_at": created_at,
        "kind": KIND_BLOSSOM_AUTH,
        "tags": tags,
        "content": content,
        "sig": sig,
    });
    let auth_b64 = base64::engine::general_purpose::STANDARD
        .encode(serde_json::to_string(&event).map_err(|e| e.to_string())?);

    let endpoint = format!("{}/upload", server.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp = client
        .put(&endpoint)
        .header("Authorization", format!("Nostr {auth_b64}"))
        .header("Content-Type", mime)
        .body(bytes)
        .send()
        .await
        .map_err(|e| format!("upload request failed: {e}"))?;

    if !resp.status().is_success() {
        let code = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let body: String = body.chars().take(200).collect();
        return Err(format!("blossom upload rejected ({code}): {body}"));
    }

    let desc: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("bad blossom response: {e}"))?;
    desc.get("url")
        .and_then(|u| u.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "blossom response missing url".to_string())
}
