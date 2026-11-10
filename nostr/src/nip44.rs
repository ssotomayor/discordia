//! Hand-written crypto, checked against the spec's own test vectors — which
//! is what makes this a checked claim rather than an assurance.

use base64::Engine as _;
use chacha20::cipher::{KeyIvInit, StreamCipher};
use hmac::{Hmac, Mac};
use sha2::Sha256;

const SALT: &[u8] = b"nip44-v2";

const VERSION: u8 = 2;

const MIN_PLAINTEXT: usize = 1;
const MAX_PLAINTEXT: usize = 65535;

const MIN_PAYLOAD: usize = 1 + 32 + (2 + 32) + 32;
const MAX_PAYLOAD: usize = 1 + 32 + (2 + 65536) + 32;

type HmacSha256 = Hmac<Sha256>;

pub fn conversation_key(
    secret: &secp256k1::SecretKey,
    their_pubkey_hex: &str,
) -> Result<[u8; 32], String> {
    let their_bytes = hex::decode(their_pubkey_hex).map_err(|e| format!("pubkey not hex: {e}"))?;
    if their_bytes.len() != 32 {
        return Err("a nostr pubkey is 32 bytes".into());
    }
    let mut compressed = [0u8; 33];
    compressed[0] = 0x02;
    compressed[1..].copy_from_slice(&their_bytes);
    let point = secp256k1::PublicKey::from_slice(&compressed)
        .map_err(|e| format!("not a point on the curve: {e}"))?;
    let xy = secp256k1::ecdh::shared_secret_point(&point, secret);
    let (prk, _) = hkdf::Hkdf::<Sha256>::extract(Some(SALT), &xy[..32]);
    Ok(prk.into())
}

fn message_keys(conversation_key: &[u8; 32], nonce: &[u8; 32]) -> ([u8; 32], [u8; 12], [u8; 32]) {
    let hk = hkdf::Hkdf::<Sha256>::from_prk(conversation_key).expect("32 bytes is a valid PRK");
    let mut okm = [0u8; 76];
    hk.expand(nonce, &mut okm)
        .expect("76 bytes is under the HKDF limit");
    let mut chacha_key = [0u8; 32];
    let mut chacha_nonce = [0u8; 12];
    let mut hmac_key = [0u8; 32];
    chacha_key.copy_from_slice(&okm[0..32]);
    chacha_nonce.copy_from_slice(&okm[32..44]);
    hmac_key.copy_from_slice(&okm[44..76]);
    (chacha_key, chacha_nonce, hmac_key)
}

fn calc_padded_len(unpadded_len: usize) -> usize {
    if unpadded_len <= 32 {
        return 32;
    }
    let next_power = 1usize << ((unpadded_len - 1).ilog2() + 1);
    let chunk = if next_power <= 256 {
        32
    } else {
        next_power / 8
    };
    chunk * ((unpadded_len - 1) / chunk + 1)
}

fn pad(plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let len = plaintext.len();
    if !(MIN_PLAINTEXT..=MAX_PLAINTEXT).contains(&len) {
        return Err(format!(
            "a NIP-44 message is 1..={MAX_PLAINTEXT} bytes; this one is {len}"
        ));
    }
    let mut out = Vec::with_capacity(2 + calc_padded_len(len));
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.extend_from_slice(plaintext);
    out.resize(2 + calc_padded_len(len), 0);
    Ok(out)
}

fn unpad(padded: &[u8]) -> Result<String, String> {
    if padded.len() < 2 {
        return Err("padded plaintext is too short to carry a length".into());
    }
    let len = u16::from_be_bytes([padded[0], padded[1]]) as usize;
    let body = padded
        .get(2..2 + len)
        .ok_or("length prefix overruns the payload")?;
    if len < MIN_PLAINTEXT || padded.len() != 2 + calc_padded_len(len) {
        return Err("padding does not match the declared length".into());
    }
    String::from_utf8(body.to_vec()).map_err(|_| "plaintext is not valid UTF-8".to_string())
}

fn encrypt_with_nonce(
    conversation_key: &[u8; 32],
    nonce: &[u8; 32],
    plaintext: &str,
) -> Result<String, String> {
    let (chacha_key, chacha_nonce, hmac_key) = message_keys(conversation_key, nonce);
    let mut buf = pad(plaintext.as_bytes())?;
    chacha20::ChaCha20::new(&chacha_key.into(), &chacha_nonce.into()).apply_keystream(&mut buf);

    let mut mac = HmacSha256::new_from_slice(&hmac_key).expect("hmac takes any key length");
    mac.update(nonce);
    mac.update(&buf);
    let tag = mac.finalize().into_bytes();

    let mut payload = Vec::with_capacity(1 + 32 + buf.len() + 32);
    payload.push(VERSION);
    payload.extend_from_slice(nonce);
    payload.extend_from_slice(&buf);
    payload.extend_from_slice(&tag);
    Ok(base64::engine::general_purpose::STANDARD.encode(payload))
}

pub fn encrypt(conversation_key: &[u8; 32], plaintext: &str) -> Result<String, String> {
    use rand::RngCore;
    let mut nonce = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    encrypt_with_nonce(conversation_key, &nonce, plaintext)
}

pub fn decrypt(conversation_key: &[u8; 32], payload: &str) -> Result<String, String> {
    if payload.starts_with('#') {
        return Err("this message uses an encryption version this build cannot read".into());
    }
    let raw = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|e| format!("payload is not base64: {e}"))?;
    if !(MIN_PAYLOAD..=MAX_PAYLOAD).contains(&raw.len()) {
        return Err(format!("payload length {} is out of range", raw.len()));
    }
    if raw[0] != VERSION {
        return Err(format!(
            "payload is encryption version {}; this build understands {VERSION}",
            raw[0]
        ));
    }
    let nonce: [u8; 32] = raw[1..33].try_into().expect("checked length");
    let ciphertext = &raw[33..raw.len() - 32];
    let tag = &raw[raw.len() - 32..];

    let (chacha_key, chacha_nonce, hmac_key) = message_keys(conversation_key, &nonce);
    let mut mac = HmacSha256::new_from_slice(&hmac_key).expect("hmac takes any key length");
    mac.update(&nonce);
    mac.update(ciphertext);
    mac.verify_slice(tag)
        .map_err(|_| "could not decrypt — wrong key, or the message was altered".to_string())?;

    let mut buf = ciphertext.to_vec();
    chacha20::ChaCha20::new(&chacha_key.into(), &chacha_nonce.into()).apply_keystream(&mut buf);
    unpad(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    const VECTORS: &str = include_str!("nip44.vectors.json");

    fn vectors() -> serde_json::Value {
        serde_json::from_str::<serde_json::Value>(VECTORS).expect("vectors parse")["v2"].clone()
    }

    fn hex32(v: &serde_json::Value) -> [u8; 32] {
        hex::decode(v.as_str().expect("hex string"))
            .expect("valid hex")
            .try_into()
            .expect("32 bytes")
    }

    fn xonly_of(sk: &secp256k1::SecretKey) -> String {
        let secp = secp256k1::Secp256k1::new();
        hex::encode(sk.x_only_public_key(&secp).0.serialize())
    }

    fn seckey(v: &serde_json::Value) -> secp256k1::SecretKey {
        secp256k1::SecretKey::from_slice(&hex32(v)).expect("valid secret key")
    }

    #[test]
    fn conversation_keys_match_the_spec() {
        for (i, t) in vectors()["valid"]["get_conversation_key"]
            .as_array()
            .expect("array")
            .iter()
            .enumerate()
        {
            let got = conversation_key(&seckey(&t["sec1"]), t["pub2"].as_str().expect("hex"))
                .unwrap_or_else(|e| panic!("vector {i}: {e}"));
            assert_eq!(
                hex::encode(got),
                t["conversation_key"].as_str().expect("hex"),
                "vector {i}"
            );
        }
    }

    #[test]
    fn message_keys_match_the_spec() {
        let v = vectors();
        let ck = hex32(&v["valid"]["get_message_keys"]["conversation_key"]);
        for (i, t) in v["valid"]["get_message_keys"]["keys"]
            .as_array()
            .expect("array")
            .iter()
            .enumerate()
        {
            let (key, nonce, hmac) = message_keys(&ck, &hex32(&t["nonce"]));
            assert_eq!(
                hex::encode(key),
                t["chacha_key"].as_str().expect("hex"),
                "chacha_key {i}"
            );
            assert_eq!(
                hex::encode(nonce),
                t["chacha_nonce"].as_str().expect("hex"),
                "chacha_nonce {i}"
            );
            assert_eq!(
                hex::encode(hmac),
                t["hmac_key"].as_str().expect("hex"),
                "hmac_key {i}"
            );
        }
    }

    #[test]
    fn padded_lengths_match_the_spec() {
        for pair in vectors()["valid"]["calc_padded_len"]
            .as_array()
            .expect("array")
        {
            let (unpadded, padded) = (
                pair[0].as_u64().expect("int") as usize,
                pair[1].as_u64().expect("int") as usize,
            );
            assert_eq!(
                calc_padded_len(unpadded),
                padded,
                "calc_padded_len({unpadded})"
            );
        }
    }

    #[test]
    fn encrypt_and_decrypt_match_the_spec() {
        for (i, t) in vectors()["valid"]["encrypt_decrypt"]
            .as_array()
            .expect("array")
            .iter()
            .enumerate()
        {
            let ck = conversation_key(&seckey(&t["sec1"]), &xonly_of(&seckey(&t["sec2"])))
                .unwrap_or_else(|e| panic!("vector {i}: {e}"));
            assert_eq!(
                hex::encode(ck),
                t["conversation_key"].as_str().expect("hex"),
                "conversation key {i}"
            );
            let mirrored = conversation_key(&seckey(&t["sec2"]), &xonly_of(&seckey(&t["sec1"])))
                .unwrap_or_else(|e| panic!("vector {i}: {e}"));
            assert_eq!(
                ck, mirrored,
                "conversation key is not symmetric, vector {i}"
            );
            let plaintext = t["plaintext"].as_str().expect("string");
            let payload = encrypt_with_nonce(&ck, &hex32(&t["nonce"]), plaintext)
                .unwrap_or_else(|e| panic!("vector {i}: {e}"));
            assert_eq!(
                payload,
                t["payload"].as_str().expect("string"),
                "payload {i}"
            );
            assert_eq!(
                decrypt(&ck, &payload).expect("decrypt"),
                plaintext,
                "round trip {i}"
            );
        }
    }

    #[test]
    fn long_messages_match_the_spec() {
        for (i, t) in vectors()["valid"]["encrypt_decrypt_long_msg"]
            .as_array()
            .expect("array")
            .iter()
            .enumerate()
        {
            let ck = hex32(&t["conversation_key"]);
            let plaintext = t["pattern"]
                .as_str()
                .expect("string")
                .repeat(t["repeat"].as_u64().expect("int") as usize);
            assert_eq!(
                hex::encode(sha2::Sha256::digest(plaintext.as_bytes())),
                t["plaintext_sha256"].as_str().expect("hex"),
                "plaintext {i}"
            );
            let payload = encrypt_with_nonce(&ck, &hex32(&t["nonce"]), &plaintext)
                .unwrap_or_else(|e| panic!("vector {i}: {e}"));
            assert_eq!(
                hex::encode(sha2::Sha256::digest(payload.as_bytes())),
                t["payload_sha256"].as_str().expect("hex"),
                "payload {i}"
            );
            assert_eq!(
                decrypt(&ck, &payload).expect("decrypt"),
                plaintext,
                "round trip {i}"
            );
        }
    }

    #[test]
    fn the_spec_s_invalid_payloads_are_all_refused() {
        let v = vectors();
        for (i, t) in v["invalid"]["decrypt"]
            .as_array()
            .expect("array")
            .iter()
            .enumerate()
        {
            let ck = hex32(&t["conversation_key"]);
            let payload = t["payload"].as_str().expect("string");
            assert!(
                decrypt(&ck, payload).is_err(),
                "vector {i} should have been refused ({})",
                t["note"].as_str().unwrap_or("no note")
            );
        }
        for (i, t) in v["invalid"]["encrypt_msg_lengths"]
            .as_array()
            .expect("array")
            .iter()
            .enumerate()
        {
            let len = t.as_u64().expect("int") as usize;
            assert!(
                encrypt_with_nonce(&[1u8; 32], &[2u8; 32], &"a".repeat(len)).is_err(),
                "length {len} (vector {i}) should have been refused"
            );
        }
        for (i, t) in v["invalid"]["get_conversation_key"]
            .as_array()
            .expect("array")
            .iter()
            .enumerate()
        {
            let sec = hex::decode(t["sec1"].as_str().expect("hex")).expect("hex");
            let refused = match secp256k1::SecretKey::from_slice(&sec) {
                Ok(sk) => conversation_key(&sk, t["pub2"].as_str().expect("hex")).is_err(),
                Err(_) => true,
            };
            assert!(
                refused,
                "vector {i} should have been refused ({})",
                t["note"].as_str().unwrap_or("no note")
            );
        }
    }

    #[test]
    fn the_same_message_encrypts_differently_each_time() {
        let ck = [7u8; 32];
        let a = encrypt(&ck, "same words").expect("encrypt");
        let b = encrypt(&ck, "same words").expect("encrypt");
        assert_ne!(a, b, "a reused nonce would reveal the xor of two messages");
        assert_eq!(decrypt(&ck, &a).expect("decrypt"), "same words");
        assert_eq!(decrypt(&ck, &b).expect("decrypt"), "same words");
    }
}
