//! Ed25519 signature verification for the `Identify` handshake.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rand::RngCore;

const NONCE_LEN: usize = 32;

/// Generate a fresh per-connection nonce (32 random bytes, base58 encoded).
pub fn fresh_nonce() -> String {
    let mut bytes = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bs58::encode(bytes).into_string()
}

/// Verify that `signature` is a valid Ed25519 signature over
/// `nonce || pubkey || username` produced by the private key matching
/// `pubkey`. All three inputs are base58 strings (pubkey + signature) or
/// raw strings (nonce + username) — they're concatenated as UTF-8 bytes.
pub fn verify_identify(
    pubkey_b58: &str,
    signature_b58: &str,
    nonce: &str,
    username: &str,
) -> Result<(), String> {
    let pubkey_bytes: [u8; 32] = bs58::decode(pubkey_b58)
        .into_vec()
        .map_err(|e| format!("pubkey not base58: {e}"))?
        .try_into()
        .map_err(|_| "pubkey is not 32 bytes".to_string())?;
    let verifying = VerifyingKey::from_bytes(&pubkey_bytes)
        .map_err(|e| format!("invalid ed25519 pubkey: {e}"))?;

    let sig_bytes: Vec<u8> = bs58::decode(signature_b58)
        .into_vec()
        .map_err(|e| format!("signature not base58: {e}"))?;
    if sig_bytes.len() != 64 {
        return Err(format!("signature is {} bytes, expected 64", sig_bytes.len()));
    }
    let sig_arr: [u8; 64] = sig_bytes.try_into().unwrap();
    let sig = Signature::from_bytes(&sig_arr);

    let mut message = Vec::with_capacity(nonce.len() + pubkey_b58.len() + username.len());
    message.extend_from_slice(nonce.as_bytes());
    message.extend_from_slice(pubkey_b58.as_bytes());
    message.extend_from_slice(username.as_bytes());

    verifying
        .verify(&message, &sig)
        .map_err(|e| format!("signature did not verify: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;

    fn random_key() -> SigningKey {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        SigningKey::from_bytes(&bytes)
    }

    #[test]
    fn good_signature_verifies() {
        let sk = random_key();
        let pubkey = bs58::encode(sk.verifying_key().as_bytes()).into_string();
        let nonce = "test-nonce";
        let username = "alice";
        let mut message = Vec::new();
        message.extend_from_slice(nonce.as_bytes());
        message.extend_from_slice(pubkey.as_bytes());
        message.extend_from_slice(username.as_bytes());
        let sig = sk.sign(&message);
        let sig_b58 = bs58::encode(sig.to_bytes()).into_string();
        assert!(verify_identify(&pubkey, &sig_b58, nonce, username).is_ok());
    }

    #[test]
    fn tampered_username_fails() {
        let sk = random_key();
        let pubkey = bs58::encode(sk.verifying_key().as_bytes()).into_string();
        let nonce = "test-nonce";
        let mut message = Vec::new();
        message.extend_from_slice(nonce.as_bytes());
        message.extend_from_slice(pubkey.as_bytes());
        message.extend_from_slice(b"alice");
        let sig = sk.sign(&message);
        let sig_b58 = bs58::encode(sig.to_bytes()).into_string();
        // Sign as alice but claim to be bob — must fail.
        assert!(verify_identify(&pubkey, &sig_b58, nonce, "bob").is_err());
    }

    #[test]
    fn wrong_pubkey_fails() {
        let sk = random_key();
        let other_pubkey = bs58::encode(random_key().verifying_key().as_bytes()).into_string();
        let nonce = "test-nonce";
        let username = "alice";
        let mut message = Vec::new();
        message.extend_from_slice(nonce.as_bytes());
        message.extend_from_slice(other_pubkey.as_bytes());
        message.extend_from_slice(username.as_bytes());
        let sig = sk.sign(&message);
        let sig_b58 = bs58::encode(sig.to_bytes()).into_string();
        assert!(verify_identify(&other_pubkey, &sig_b58, nonce, username).is_err());
    }

    #[test]
    fn fresh_nonce_changes() {
        let a = fresh_nonce();
        let b = fresh_nonce();
        assert_ne!(a, b);
        assert!(a.len() > 30);
    }
}
