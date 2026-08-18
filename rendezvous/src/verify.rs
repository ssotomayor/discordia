//! Ownership proof for claimed server names.
//!
//! A host that claims a persistent name must prove control of the Nostr key it
//! binds the name to — otherwise anyone could squat a name across a restart
//! (pubkeys are public). Same scheme as the server's `Identify`: the rendezvous
//! issues a per-connection nonce, the host returns a Schnorr (BIP-340)
//! signature over `SHA256(nonce || pubkey_hex || name)`.

use rand::RngCore;
use secp256k1::schnorr::Signature;
use secp256k1::{Message, Secp256k1, XOnlyPublicKey};
use sha2::{Digest, Sha256};

const NONCE_LEN: usize = 32;

/// Fresh per-connection nonce (32 random bytes, base58).
pub fn fresh_nonce() -> String {
    let mut bytes = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bs58::encode(bytes).into_string()
}

/// Verify that `signature` is a valid Schnorr signature over
/// `SHA256(nonce || pubkey_hex || bound)` from the key matching `pubkey_hex`
/// (64-char hex x-only Nostr pubkey; `signature_hex` is 128-char hex).
///
/// `bound` is whatever the key is vouching for. Two things use this: the
/// claimed *name*, and the *transport key* a host publishes for the QUIC
/// path — different payloads, identical proof, and the nonce is what stops
/// either signature being replayed as the other on a later connection.
pub fn verify_ownership(
    pubkey_hex: &str,
    signature_hex: &str,
    nonce: &str,
    bound: &str,
) -> Result<(), String> {
    let pubkey_bytes = hex::decode(pubkey_hex).map_err(|e| format!("pubkey not hex: {e}"))?;
    let xonly = XOnlyPublicKey::from_slice(&pubkey_bytes)
        .map_err(|e| format!("invalid nostr pubkey: {e}"))?;

    let sig_bytes = hex::decode(signature_hex).map_err(|e| format!("signature not hex: {e}"))?;
    let sig =
        Signature::from_slice(&sig_bytes).map_err(|e| format!("invalid schnorr signature: {e}"))?;

    let mut message = Vec::with_capacity(nonce.len() + pubkey_hex.len() + bound.len());
    message.extend_from_slice(nonce.as_bytes());
    message.extend_from_slice(pubkey_hex.as_bytes());
    message.extend_from_slice(bound.as_bytes());
    let digest: [u8; 32] = Sha256::digest(&message).into();
    let msg = Message::from_digest(digest);

    let secp = Secp256k1::verification_only();
    secp.verify_schnorr(&sig, &msg, &xonly)
        .map_err(|e| format!("ownership signature did not verify: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secp256k1::{Keypair, SecretKey};

    fn sign(secret: &SecretKey, nonce: &str, pubkey_hex: &str, name: &str) -> String {
        let secp = Secp256k1::new();
        let keypair = Keypair::from_secret_key(&secp, secret);
        let mut message = Vec::new();
        message.extend_from_slice(nonce.as_bytes());
        message.extend_from_slice(pubkey_hex.as_bytes());
        message.extend_from_slice(name.as_bytes());
        let digest: [u8; 32] = Sha256::digest(&message).into();
        let msg = Message::from_digest(digest);
        hex::encode(secp.sign_schnorr_no_aux_rand(&msg, &keypair).serialize())
    }

    #[test]
    fn valid_signature_verifies_and_tampering_fails() {
        let secp = Secp256k1::new();
        let secret = SecretKey::from_slice(&[7u8; 32]).unwrap();
        let keypair = Keypair::from_secret_key(&secp, &secret);
        let (xonly, _) = keypair.x_only_public_key();
        let pubkey_hex = hex::encode(xonly.serialize());
        let nonce = fresh_nonce();

        let sig = sign(&secret, &nonce, &pubkey_hex, "acme");
        assert!(verify_ownership(&pubkey_hex, &sig, &nonce, "acme").is_ok());
        assert!(verify_ownership(&pubkey_hex, &sig, &nonce, "evil").is_err());
        assert!(verify_ownership(&pubkey_hex, &sig, "other-nonce", "acme").is_err());
    }
}
