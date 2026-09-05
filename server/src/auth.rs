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
