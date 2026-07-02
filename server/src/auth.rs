//! Schnorr (BIP-340 / Nostr) signature verification for the `Identify`
//! handshake.

use rand::RngCore;
use secp256k1::schnorr::Signature;
use secp256k1::{Message, Secp256k1, XOnlyPublicKey};
use sha2::{Digest, Sha256};

const NONCE_LEN: usize = 32;

/// Generate a fresh per-connection nonce (32 random bytes, base58 encoded).
pub fn fresh_nonce() -> String {
    let mut bytes = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bs58::encode(bytes).into_string()
}

/// Verify that `signature` is a valid Schnorr signature over
/// `SHA256(nonce || pubkey_hex || username)` from the key matching `pubkey`.
/// `pubkey` is a 64-char hex x-only Nostr pubkey; `signature` is 128-char hex.
pub fn verify_identify(
    pubkey_hex: &str,
    signature_hex: &str,
    nonce: &str,
    username: &str,
) -> Result<(), String> {
    let pubkey_bytes = hex::decode(pubkey_hex).map_err(|e| format!("pubkey not hex: {e}"))?;
    let xonly = XOnlyPublicKey::from_slice(&pubkey_bytes)
        .map_err(|e| format!("invalid nostr pubkey: {e}"))?;

    let sig_bytes = hex::decode(signature_hex).map_err(|e| format!("signature not hex: {e}"))?;
    let sig =
        Signature::from_slice(&sig_bytes).map_err(|e| format!("invalid schnorr signature: {e}"))?;

    let mut message = Vec::with_capacity(nonce.len() + pubkey_hex.len() + username.len());
    message.extend_from_slice(nonce.as_bytes());
    message.extend_from_slice(pubkey_hex.as_bytes());
    message.extend_from_slice(username.as_bytes());
    let digest: [u8; 32] = Sha256::digest(&message).into();
    let msg = Message::from_digest(digest);

    let secp = Secp256k1::verification_only();
    secp.verify_schnorr(&sig, &msg, &xonly)
        .map_err(|e| format!("signature did not verify: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secp256k1::{Keypair, SecretKey};

    fn sign(secret: &SecretKey, nonce: &str, pubkey_hex: &str, username: &str) -> String {
        let secp = Secp256k1::new();
        let keypair = Keypair::from_secret_key(&secp, secret);
        let mut message = Vec::new();
        message.extend_from_slice(nonce.as_bytes());
        message.extend_from_slice(pubkey_hex.as_bytes());
        message.extend_from_slice(username.as_bytes());
        let digest: [u8; 32] = Sha256::digest(&message).into();
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
        let (nonce, username) = ("test-nonce", "alice");
        let sig = sign(&secret, nonce, &pubkey, username);
        assert!(verify_identify(&pubkey, &sig, nonce, username).is_ok());
    }

    #[test]
    fn tampered_username_fails() {
        let (secret, pubkey) = keypair();
        let sig = sign(&secret, "n", &pubkey, "alice");
        assert!(verify_identify(&pubkey, &sig, "n", "mallory").is_err());
    }
}
