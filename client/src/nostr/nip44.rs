//! NIP-44 v2 — the encryption every Nostr direct message is built on.
//!
//! This is the payload layer: it turns a string and a pair of keys into an
//! opaque base64 blob and back. It knows nothing about events, relays or
//! conversations — NIP-59 wraps it and NIP-17 gives it meaning.
//!
//! **Why this is written out rather than pulled in.** Implementing a crypto
//! spec by hand is normally the wrong instinct, and it is worth saying why it
//! is not here. Every primitive was already compiled into this client —
//! secp256k1, HKDF-SHA256, ChaCha20, HMAC-SHA256, base64 — so the alternative
//! bought no safety, only a second `secp256k1` beside the pinned one and a
//! bridge from `Identity` to somebody else's key type. And the spec ships
//! **official test vectors**, which is the thing that actually matters: the
//! module is checked against 35 conversation keys, 32 message-key expansions,
//! 24 padding lengths, 13 encrypt/decrypt round trips and 24 rejection cases
//! that upstream wrote to catch exactly the mistakes a reimplementation makes.
//! A claim that this is correct is a test result, not an assurance. The same
//! house rule as `identity.rs`'s NIP-06 and `blossom.rs`'s NIP-01.
//!
//! **What it does not give you.** The conversation key is derived from two
//! static identity keys, so there is no forward secrecy: whoever obtains a
//! private key can read every message it ever took part in, past included.
//! That is a property of NIP-44 itself and not of this code, and it is the
//! reason the padding below matters more than it looks — length is the one
//! thing an attacker gets for free, so the spec buys it back in buckets.

// Nothing in the binary calls this yet: it is the first slice of the Nostr DM
// work, and `nip59`/`nip17` are what will reach it. Every function here is
// exercised by the vector tests below, so this is code that is *unreached*
// rather than unproven — and the attribute comes off the moment a caller lands.
// It is scoped to this module rather than the crate so it cannot quietly cover
// anything else in the meantime.

use base64::Engine as _;
use chacha20::cipher::{KeyIvInit, StreamCipher};
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Salt for the conversation-key extraction. Also the version marker: a v3
/// would use a different string and so derive an unrelated key, which is what
/// stops a downgrade from being merely a version-byte edit.
const SALT: &[u8] = b"nip44-v2";

/// The only payload version this understands.
const VERSION: u8 = 2;

/// Longest plaintext v2 will carry, in bytes of UTF-8.
///
/// The ceiling is the `u16` length prefix, and the floor is 1: an empty
/// message is rejected rather than encrypted, because a zero-length plaintext
/// is indistinguishable from a decryption that produced nothing.
const MIN_PLAINTEXT: usize = 1;
const MAX_PLAINTEXT: usize = 65535;

/// Smallest and largest a well-formed payload can be, before base64.
///
/// Checked before anything is parsed. A length test is not a security control
/// on its own — the MAC is — but it is what turns a malformed blob into an
/// error instead of a slice past the end of a buffer.
///
/// **The ciphertext is the padded plaintext, and padding includes the 2-byte
/// length prefix.** Getting that wrong is not a hypothetical: the first version
/// of these bounds omitted it and this module rejected its own output at the
/// maximum message size — encryption produced a byte-exact match for the
/// spec's longest vector, and then decryption refused to read it.
const MIN_PAYLOAD: usize = 1 + 32 + (2 + 32) + 32;
const MAX_PAYLOAD: usize = 1 + 32 + (2 + 65536) + 32;

type HmacSha256 = Hmac<Sha256>;

/// The long-lived secret two identities share, from which every message key is
/// expanded.
///
/// **The x coordinate goes in unhashed.** ECDH here yields a point; the spec
/// takes its 32-byte x coordinate raw and hands it to HKDF as input keying
/// material, rather than hashing it first the way `Identity::shared_secret_with`
/// does for our own media keys. Hashing first would be just as safe and would
/// interoperate with nothing, which is the whole point of following the spec
/// rather than our own precedent.
///
/// Deriving this is the expensive half of a message (a scalar multiplication),
/// and it is the same for every message between two people — so a caller
/// sending a run of them should derive once and reuse.
pub fn conversation_key(
    secret: &secp256k1::SecretKey,
    their_pubkey_hex: &str,
) -> Result<[u8; 32], String> {
    let their_bytes = hex::decode(their_pubkey_hex).map_err(|e| format!("pubkey not hex: {e}"))?;
    if their_bytes.len() != 32 {
        return Err("a nostr pubkey is 32 bytes".into());
    }
    // Even parity (0x02) is the standard Nostr convention; both sides must
    // match or ECDH fails.
    let mut compressed = [0u8; 33];
    compressed[0] = 0x02;
    compressed[1..].copy_from_slice(&their_bytes);
    let point = secp256k1::PublicKey::from_slice(&compressed)
        .map_err(|e| format!("not a point on the curve: {e}"))?;
    let xy = secp256k1::ecdh::shared_secret_point(&point, secret);
    let (prk, _) = hkdf::Hkdf::<Sha256>::extract(Some(SALT), &xy[..32]);
    Ok(prk.into())
}

/// Per-message keys, expanded from the conversation key and this message's
/// nonce.
///
/// Returned as a triple rather than a struct because that is how the spec's own
/// test vectors are laid out, and matching them exactly is what lets the test
/// below compare fields one by one instead of comparing a blob and guessing
/// which part went wrong.
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

/// How long the padded plaintext region is for a message of `unpadded_len`.
///
/// Buckets, not exact lengths, because the ciphertext length is public and a
/// length is a surprising amount of information — "yes"/"no" are
/// distinguishable, and so is a pasted key from a sentence. Everything up to 32
/// bytes shares one bucket; above that the bucket is a power of two divided
/// into eighths, so the leak is bounded to roughly an eighth of the message
/// size rather than a byte.
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

/// `[len: u16 BE][plaintext][zeros]`, the length prefix being what makes the
/// padding removable without trusting the zeros.
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

/// Undo `pad`, refusing anything whose prefix does not describe what arrived.
///
/// Every check here is load-bearing and each one corresponds to a rejection
/// case in the official vectors: a prefix longer than the buffer would slice
/// out of bounds, and a padded length that disagrees with the prefix means the
/// padding was rewritten — which the MAC should already have caught, so
/// reaching this branch at all is a sign something is wrong upstream.
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

/// Encrypt `plaintext` under a conversation key and an explicit nonce.
///
/// Separate from `encrypt` so the test vectors — which fix the nonce in order
/// to compare payloads byte for byte — can drive the same code the real path
/// uses. Nothing outside a test should choose its own nonce: reusing one under
/// the same conversation key reveals the XOR of two messages.
fn encrypt_with_nonce(
    conversation_key: &[u8; 32],
    nonce: &[u8; 32],
    plaintext: &str,
) -> Result<String, String> {
    let (chacha_key, chacha_nonce, hmac_key) = message_keys(conversation_key, nonce);
    let mut buf = pad(plaintext.as_bytes())?;
    chacha20::ChaCha20::new(&chacha_key.into(), &chacha_nonce.into()).apply_keystream(&mut buf);

    // MAC over nonce ‖ ciphertext, not ciphertext alone. The nonce is what
    // selects the keys, so leaving it unauthenticated would let it be swapped
    // for another the recipient also has keys for.
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

/// Encrypt `plaintext`, choosing a fresh random nonce.
pub fn encrypt(conversation_key: &[u8; 32], plaintext: &str) -> Result<String, String> {
    use rand::RngCore;
    let mut nonce = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    encrypt_with_nonce(conversation_key, &nonce, plaintext)
}

/// Decrypt a payload, or say why it could not be trusted.
///
/// Ordinary failure, not exceptional: a message sealed to a key we no longer
/// hold, or by a client speaking a version we do not, is something the UI has
/// to render rather than something to panic over.
pub fn decrypt(conversation_key: &[u8; 32], payload: &str) -> Result<String, String> {
    // A leading '#' is the spec's reservation for a future non-base64 encoding.
    // Rejecting it by name gives a better answer than "invalid base64" to
    // somebody whose client is simply newer than this one.
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
    // Verify before decrypting to avoid processing attacker-chosen bytes; use
    // constant-time compare to prevent timing leaks.
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

    /// The official vectors, committed rather than fetched.
    ///
    /// A test that reaches the network is a test that fails when GitHub does,
    /// and this one has to run in CI where nothing may. 37 KB, and only ever
    /// compiled into the test binary.
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

    /// x-only public key of a secret, as 64-char hex — what a Nostr pubkey is.
    fn xonly_of(sk: &secp256k1::SecretKey) -> String {
        let secp = secp256k1::Secp256k1::new();
        hex::encode(sk.x_only_public_key(&secp).0.serialize())
    }

    fn seckey(v: &serde_json::Value) -> secp256k1::SecretKey {
        secp256k1::SecretKey::from_slice(&hex32(v)).expect("valid secret key")
    }

    /// 35 vectors. This is the step where an implementation most often goes
    /// quietly wrong — hashing the shared point, or taking x‖y instead of x —
    /// and the failure mode is a key that works with nobody.
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

    /// 32 vectors over the HKDF-expand split. Compared field by field, so a
    /// wrong boundary names which of the three it moved.
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

    /// 24 vectors. The bucket boundaries are where an off-by-one hides, and a
    /// wrong bucket is not a crash — it is a payload the other side rejects.
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

    /// The whole thing, byte for byte, against a fixed nonce — and then back
    /// again. Both directions matter: matching payloads proves we encrypt like
    /// everyone else, and decrypting proves we can read what they send.
    #[test]
    fn encrypt_and_decrypt_match_the_spec() {
        for (i, t) in vectors()["valid"]["encrypt_decrypt"]
            .as_array()
            .expect("array")
            .iter()
            .enumerate()
        {
            // These vectors give both *secrets* rather than a secret and a
            // pubkey, so the pubkey is derived here — which incidentally checks
            // that our x-only derivation agrees with the one that produced
            // them, a step the conversation-key vectors take for granted.
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

    /// The three long ones, up to the 65535-byte ceiling, compared by hash
    /// because the vectors do not carry a megabyte of expected output.
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

    /// The rejection cases, which are the half a reimplementation is most
    /// likely to skip: a tampered MAC, a bad version, an impossible length, a
    /// payload that is not base64. Every one of these must be an error rather
    /// than a plausible-looking string.
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

    /// A random nonce every time, so the same message twice is two payloads.
    /// Not covered by the vectors, which necessarily fix the nonce.
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
