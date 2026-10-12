use rand::RngCore;
use secp256k1::schnorr::Signature;
use secp256k1::{Message, Secp256k1, XOnlyPublicKey};
use sha2::{Digest, Sha256};

const NONCE_LEN: usize = 32;

pub fn fresh_nonce() -> String {
    let mut bytes = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bs58::encode(bytes).into_string()
}

/// `origin` is the address the client dialed. Without it in the signature, a
/// server you connect to can forward another server's nonce and log in there
/// as you; with it, that signature names an address the other server is not.
pub fn verify_identify(
    pubkey_hex: &str,
    signature_hex: &str,
    nonce: &str,
    origin: &str,
    username: &str,
) -> Result<(), String> {
    let pubkey_bytes = hex::decode(pubkey_hex).map_err(|e| format!("pubkey not hex: {e}"))?;
    let xonly = XOnlyPublicKey::from_slice(&pubkey_bytes)
        .map_err(|e| format!("invalid nostr pubkey: {e}"))?;

    let sig_bytes = hex::decode(signature_hex).map_err(|e| format!("signature not hex: {e}"))?;
    let sig =
        Signature::from_slice(&sig_bytes).map_err(|e| format!("invalid schnorr signature: {e}"))?;

    let payload = crate::protocol::identify_payload(nonce, origin, pubkey_hex, username);
    let digest: [u8; 32] = Sha256::digest(&payload).into();
    let msg = Message::from_digest(digest);

    let secp = Secp256k1::verification_only();
    secp.verify_schnorr(&sig, &msg, &xonly)
        .map_err(|e| format!("signature did not verify: {e}"))
}

pub struct VerifiedManifest {
    pub pubkey: String,
    pub manifest: crate::protocol::AppManifest,
    pub event: String,
}

#[derive(serde::Deserialize)]
struct NostrEvent {
    id: String,
    pubkey: String,
    created_at: i64,
    kind: u32,
    tags: Vec<Vec<String>>,
    content: String,
    sig: String,
}

/// A NIP-01 event, checked here rather than trusted from the client that
/// relayed it: the pin it becomes is what decides an app's powers.
pub fn verify_manifest_event(event_json: &str) -> Result<VerifiedManifest, String> {
    use crate::protocol::{APP_MANIFEST_D, APP_MANIFEST_KIND, MAX_MANIFEST_EVENT_BYTES};
    if event_json.len() > MAX_MANIFEST_EVENT_BYTES {
        return Err("manifest event is too large".into());
    }
    let ev: NostrEvent =
        serde_json::from_str(event_json).map_err(|e| format!("manifest is not an event: {e}"))?;
    if ev.kind != APP_MANIFEST_KIND {
        return Err(format!("manifest must be kind {APP_MANIFEST_KIND}"));
    }
    let d = ev
        .tags
        .iter()
        .find(|t| t.first().is_some_and(|n| n == "d"))
        .and_then(|t| t.get(1));
    if d.map(String::as_str) != Some(APP_MANIFEST_D) {
        return Err(format!("manifest needs the d tag {APP_MANIFEST_D}"));
    }
    let canonical = serde_json::json!([0, ev.pubkey, ev.created_at, ev.kind, ev.tags, ev.content]);
    let id: [u8; 32] = Sha256::digest(canonical.to_string().as_bytes()).into();
    if hex::encode(id) != ev.id {
        return Err("manifest id does not match its content".into());
    }
    let pubkey_bytes = hex::decode(&ev.pubkey).map_err(|e| format!("pubkey not hex: {e}"))?;
    let xonly = XOnlyPublicKey::from_slice(&pubkey_bytes)
        .map_err(|e| format!("invalid nostr pubkey: {e}"))?;
    let sig_bytes = hex::decode(&ev.sig).map_err(|e| format!("signature not hex: {e}"))?;
    let sig =
        Signature::from_slice(&sig_bytes).map_err(|e| format!("invalid schnorr signature: {e}"))?;
    Secp256k1::verification_only()
        .verify_schnorr(&sig, &Message::from_digest(id), &xonly)
        .map_err(|e| format!("manifest signature did not verify: {e}"))?;
    let manifest: crate::protocol::AppManifest = serde_json::from_str(&ev.content)
        .map_err(|e| format!("manifest content is not a manifest: {e}"))?;
    Ok(VerifiedManifest {
        pubkey: ev.pubkey,
        manifest: crate::protocol::validate_manifest(manifest)?,
        event: event_json.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use secp256k1::{Keypair, SecretKey};

    fn sign(
        secret: &SecretKey,
        nonce: &str,
        origin: &str,
        pubkey_hex: &str,
        username: &str,
    ) -> String {
        let secp = Secp256k1::new();
        let keypair = Keypair::from_secret_key(&secp, secret);
        let payload = crate::protocol::identify_payload(nonce, origin, pubkey_hex, username);
        let digest: [u8; 32] = Sha256::digest(&payload).into();
        let msg = Message::from_digest(digest);
        hex::encode(secp.sign_schnorr_no_aux_rand(&msg, &keypair).serialize())
    }

    fn keypair() -> (SecretKey, String) {
        let secp = Secp256k1::new();
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let secret = SecretKey::from_slice(&bytes).unwrap();
        let (xonly, _) = secret.x_only_public_key(&secp);
        (secret, hex::encode(xonly.serialize()))
    }

    #[test]
    fn good_signature_verifies() {
        let (secret, pubkey) = keypair();
        let (nonce, origin, username) = ("test-nonce", "127.0.0.1:9000", "alice");
        let sig = sign(&secret, nonce, origin, &pubkey, username);
        assert!(verify_identify(&pubkey, &sig, nonce, origin, username).is_ok());
    }

    #[test]
    fn tampered_username_fails() {
        let (secret, pubkey) = keypair();
        let sig = sign(&secret, "n", "h:1", &pubkey, "alice");
        assert!(verify_identify(&pubkey, &sig, "n", "h:1", "mallory").is_err());
    }

    #[test]
    fn a_signature_for_one_address_is_worthless_at_another() {
        let (secret, pubkey) = keypair();
        let sig = sign(&secret, "n", "evil.example:9000", &pubkey, "alice");
        assert!(verify_identify(&pubkey, &sig, "n", "chat.example:9000", "alice").is_err());
        assert!(verify_identify(&pubkey, &sig, "n", "evil.example:9000", "alice").is_ok());
    }

    #[test]
    fn the_old_unbound_payload_no_longer_verifies() {
        let (secret, pubkey) = keypair();
        let secp = Secp256k1::new();
        let keypair = Keypair::from_secret_key(&secp, &secret);
        let legacy = format!("n{pubkey}alice");
        let digest: [u8; 32] = Sha256::digest(legacy.as_bytes()).into();
        let sig = hex::encode(
            secp.sign_schnorr_no_aux_rand(&Message::from_digest(digest), &keypair)
                .serialize(),
        );
        assert!(verify_identify(&pubkey, &sig, "n", "", "alice").is_err());
    }
}
