//! Cryptographic identity — **Nostr** keys (secp256k1 / Schnorr, BIP-340).
//!
//! Each user is a secp256k1 keypair; the public key (x-only, 32 bytes) is the
//! universal user id, encoded as 64-char hex on the wire and shown as `npub…`
//! (NIP-19 bech32). Two ways to create one:
//!
//! 1. **BIP39 seed phrase** — generated at first launch and derived via
//!    **NIP-06** (`m/44'/1237'/0'/0/0`). The 12-word phrase is the recovery
//!    format and is interchangeable with other NIP-06 wallets.
//! 2. **Key import** — paste an `nsec1…` (bech32) or a raw 64-char hex secret.
//!
//! Persisted to `dioxusfun/identity.json` in the config dir, mode 0600 on Unix.

use std::path::PathBuf;

use bech32::{Bech32, Hrp};
use bip39::{Language, Mnemonic};
use hmac::{Hmac, Mac};
use secp256k1::{Keypair, Message, PublicKey, Scalar, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};

const FILE_VERSION: u32 = 1;

/// Where the identity came from, so recovery UI knows what to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentitySource {
    /// 12-word BIP39 mnemonic (NIP-06 derivable).
    Phrase(String),
    /// An `nsec1…` bech32 secret. No seed phrase available.
    Nsec(String),
}

/// Domain separator for channel media keys.
///
/// **Frozen.** Every media key two peers have ever agreed on is derived under
/// this label; changing it makes a client on either side of the change derive a
/// different secret and hear silence, with nothing to say why.
const MEDIA_KEY_DOMAIN: &[u8] = b"dioxusfun/media-key/v1";

#[derive(Clone)]
pub struct Identity {
    /// x-only public key as 64-char hex — the universal user id.
    pub pubkey: String,
    pub display_name: String,
    pub source: IdentitySource,
    secret: SecretKey,
}

impl PartialEq for Identity {
    fn eq(&self, other: &Self) -> bool {
        self.pubkey == other.pubkey && self.display_name == other.display_name
    }
}

impl Eq for Identity {}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("pubkey", &self.pubkey)
            .field("display_name", &self.display_name)
            .field("source", &"<redacted>")
            .finish()
    }
}

impl Identity {
    /// Generate a fresh BIP39-derived keypair (12 words / 128 bits entropy).
    pub fn create(display_name: impl Into<String>) -> Result<Self, String> {
        use rand::RngCore;
        let mut entropy = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut entropy);
        let mnemonic = Mnemonic::from_entropy_in(Language::English, &entropy)
            .map_err(|e: bip39::Error| e.to_string())?;
        Self::from_mnemonic(mnemonic, display_name.into())
    }

    /// Restore from a 12- or 24-word BIP39 phrase (NIP-06).
    pub fn restore_from_phrase(
        seed_phrase: impl AsRef<str>,
        display_name: impl Into<String>,
    ) -> Result<Self, String> {
        let mnemonic =
            Mnemonic::parse_in_normalized(Language::English, seed_phrase.as_ref().trim())
                .map_err(|e: bip39::Error| format!("invalid recovery phrase: {e}"))?;
        Self::from_mnemonic(mnemonic, display_name.into())
    }

    /// Import from an `nsec1…` bech32 secret or a raw 64-char hex secret.
    pub fn restore_from_private_key(
        input: impl AsRef<str>,
        display_name: impl Into<String>,
    ) -> Result<Self, String> {
        let raw = input.as_ref().trim();
        let secret_bytes: [u8; 32] = if raw.starts_with("nsec") {
            let (hrp, data) = bech32::decode(raw).map_err(|e| format!("invalid nsec: {e}"))?;
            if hrp.as_str() != "nsec" {
                return Err(format!("expected an nsec key, got '{}'", hrp.as_str()));
            }
            data.try_into()
                .map_err(|_| "nsec is not 32 bytes".to_string())?
        } else {
            let bytes =
                hex::decode(raw).map_err(|e| format!("private key is not hex or nsec: {e}"))?;
            bytes
                .try_into()
                .map_err(|_| "hex private key must be 32 bytes (64 chars)".to_string())?
        };
        let secret =
            SecretKey::from_slice(&secret_bytes).map_err(|e| format!("invalid secret key: {e}"))?;
        let nsec = to_bech32("nsec", &secret_bytes);
        Ok(Self::from_secret(
            secret,
            display_name.into(),
            IdentitySource::Nsec(nsec),
        ))
    }

    fn from_mnemonic(mnemonic: Mnemonic, display_name: String) -> Result<Self, String> {
        let seed: [u8; 64] = mnemonic.to_seed("");
        let secret = derive_nip06(&seed)?;
        let phrase = mnemonic.to_string();
        Ok(Self::from_secret(
            secret,
            display_name,
            IdentitySource::Phrase(phrase),
        ))
    }

    fn from_secret(secret: SecretKey, display_name: String, source: IdentitySource) -> Self {
        let secp = Secp256k1::new();
        let (xonly, _parity) = secret.x_only_public_key(&secp);
        let pubkey = hex::encode(xonly.serialize());
        Self {
            pubkey,
            display_name,
            source,
            secret,
        }
    }

    /// The raw secret, for the Nostr code that signs events with it.
    ///
    /// Deliberately narrow in intent: `nostr::event::sign_with` takes a
    /// `SecretKey` because gift wrapping also signs with *ephemeral* keys that
    /// have no `Identity` behind them, and one signing function for both is
    /// better than two that could drift. Everything else should keep going
    /// through `sign_hex`/`nostr_sign_id`.
    pub fn secret_key(&self) -> secp256k1::SecretKey {
        self.secret
    }

    /// A stable 32-byte seed for this identity's *transport* key.
    ///
    /// The QUIC transport authenticates peers by an ed25519 key, which is not
    /// the secp256k1 key that is your account — a second key, needed on a
    /// different curve. Deriving it from the first rather than generating and
    /// storing one means a host keeps the same transport identity across
    /// restarts and reinstalls, so a friend who saved your address is still
    /// reaching *you* and not a stranger who took the address over.
    ///
    /// Domain-separated so this hash can never collide with anything else we
    /// sign or derive, and one-way, so handing out the transport key tells
    /// nobody anything about the Nostr secret.
    ///
    /// The two keys are still only *asserted* to belong together — the binding
    /// is a signature over the transport key, made by the Nostr key, verified
    /// where it is published. Derivation makes the key stable; the signature is
    /// what makes it yours.
    pub fn transport_seed(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"dioxusfun/quic-transport/v1");
        h.update(self.secret.secret_bytes());
        h.finalize().into()
    }

    /// A secret this identity shares with exactly one other pubkey, and nobody
    /// else — the basis for sealing a channel's media key to one member.
    ///
    /// ECDH over secp256k1, then a domain-separated hash. Two details are not
    /// free choices:
    ///
    /// **Parity.** A Nostr pubkey is x-only: 32 bytes, with the y-coordinate
    /// dropped. Reconstructing a point needs a parity byte, and both sides must
    /// pick the same one or they derive different secrets. Even (`0x02`) is what
    /// NIP-04 and NIP-44 settled on, so it is what this uses.
    ///
    /// **The x-coordinate only.** `shared_secret_point` returns x‖y; the y half
    /// is discarded before hashing, again matching the Nostr convention. Mixing
    /// it in would be just as secure and interoperate with nothing.
    ///
    /// The hash is the domain separator's whole job: this secret must never
    /// coincide with one derived for another purpose from the same pair of keys.
    pub fn shared_secret_with(&self, their_pubkey_hex: &str) -> Result<[u8; 32], String> {
        self.secret_with_domain(their_pubkey_hex, MEDIA_KEY_DOMAIN)
    }

    /// ECDH, then a domain-separated hash.
    ///
    /// **`MEDIA_KEY_DOMAIN` must never change.** It is baked into every media
    /// key two peers have ever agreed on; altering it would mean a client on
    /// either side of the change derives a different secret and hears silence,
    /// with nothing to say why.
    fn secret_with_domain(
        &self,
        their_pubkey_hex: &str,
        domain: &[u8],
    ) -> Result<[u8; 32], String> {
        use sha2::{Digest, Sha256};

        let their_bytes =
            hex::decode(their_pubkey_hex).map_err(|e| format!("pubkey not hex: {e}"))?;
        if their_bytes.len() != 32 {
            return Err("a nostr pubkey is 32 bytes".into());
        }
        // x-only -> compressed point, even parity by convention.
        let mut compressed = [0u8; 33];
        compressed[0] = 0x02;
        compressed[1..].copy_from_slice(&their_bytes);
        let point = PublicKey::from_slice(&compressed)
            .map_err(|e| format!("not a point on the curve: {e}"))?;

        let xy = secp256k1::ecdh::shared_secret_point(&point, &self.secret);
        let mut h = Sha256::new();
        h.update(domain);
        h.update(&xy[..32]);
        Ok(h.finalize().into())
    }

    /// Schnorr-sign a message (hashed to 32 bytes with SHA-256); returns hex.
    /// The server hashes identically and verifies.
    pub fn sign_hex(&self, message: &[u8]) -> String {
        let secp = Secp256k1::new();
        let keypair = Keypair::from_secret_key(&secp, &self.secret);
        let digest: [u8; 32] = Sha256::digest(message).into();
        let msg = Message::from_digest(digest);
        let sig = secp.sign_schnorr_no_aux_rand(&msg, &keypair);
        hex::encode(sig.serialize())
    }

    /// Schnorr-sign a 32-byte Nostr event id directly (NO extra hashing — the
    /// id is already `sha256` of the serialized event). Returns hex. Used for
    /// signing Nostr events like Blossom's `kind:24242` auth.
    pub fn nostr_sign_id(&self, id: &[u8; 32]) -> String {
        let secp = Secp256k1::new();
        let keypair = Keypair::from_secret_key(&secp, &self.secret);
        let msg = Message::from_digest(*id);
        hex::encode(secp.sign_schnorr_no_aux_rand(&msg, &keypair).serialize())
    }

    /// NIP-19 `npub…` for display.
    pub fn npub(&self) -> String {
        // pubkey is valid hex of 32 bytes by construction.
        let bytes = hex::decode(&self.pubkey).unwrap_or_default();
        to_bech32("npub", &bytes)
    }

    /// NIP-19 `nsec…` for export/backup.
    #[allow(dead_code)]
    pub fn export_nsec(&self) -> String {
        to_bech32("nsec", &self.secret.secret_bytes())
    }

    pub fn save(&self) -> Result<(), String> {
        let path = identity_path();
        let parent = path
            .parent()
            .ok_or_else(|| "identity path has no parent".to_string())?;
        std::fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
        let mut stored = Stored {
            version: FILE_VERSION,
            display_name: self.display_name.clone(),
            pubkey: self.pubkey.clone(),
            seed_phrase: None,
            nsec: None,
        };
        match &self.source {
            IdentitySource::Phrase(p) => stored.seed_phrase = Some(p.clone()),
            IdentitySource::Nsec(k) => stored.nsec = Some(k.clone()),
        }
        let content = serde_json::to_string_pretty(&stored)
            .map_err(|e| format!("serialize identity: {e}"))?;
        std::fs::write(&path, content).map_err(|e| format!("write identity: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(&path, perms);
            }
        }
        Ok(())
    }

    /// Returns Ok(None) if no identity file exists.
    pub fn load() -> Result<Option<Self>, String> {
        let path = identity_path();
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path).map_err(|e| format!("read identity: {e}"))?;
        let stored: Stored =
            serde_json::from_str(&content).map_err(|e| format!("parse identity: {e}"))?;
        if stored.version != FILE_VERSION {
            return Err(format!(
                "unknown identity file version {}; expected {}",
                stored.version, FILE_VERSION
            ));
        }
        let identity = if let Some(phrase) = stored.seed_phrase {
            Self::restore_from_phrase(&phrase, stored.display_name)?
        } else if let Some(nsec) = stored.nsec {
            Self::restore_from_private_key(&nsec, stored.display_name)?
        } else {
            return Err("identity file has neither seed_phrase nor nsec".into());
        };
        Ok(Some(identity))
    }

    pub fn file_path_display() -> String {
        identity_path().display().to_string()
    }

    pub fn delete_file() -> Result<(), String> {
        let path = identity_path();
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("remove identity: {e}"))?;
        }
        Ok(())
    }

    pub fn set_display_name(&mut self, name: impl Into<String>) -> Result<(), String> {
        self.display_name = name.into();
        self.save()
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    display_name: String,
    pubkey: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seed_phrase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nsec: Option<String>,
}

fn to_bech32(hrp: &str, data: &[u8]) -> String {
    let hrp = Hrp::parse(hrp).expect("valid hrp");
    bech32::encode::<Bech32>(hrp, data).expect("bech32 encode")
}

/// Base directory for dioxusfun's on-disk state (identity + session).
///
/// Honors `DIOXUSFUN_CONFIG_DIR` (handy for running several isolated instances
/// on one machine). Otherwise the OS config dir, e.g.
/// `~/Library/Application Support/dioxusfun` on macOS.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("DIOXUSFUN_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("dioxusfun")
}

fn identity_path() -> PathBuf {
    config_dir().join("identity.json")
}

pub fn truncate_pubkey(pubkey: &str) -> String {
    if pubkey.len() <= 12 {
        return pubkey.to_string();
    }
    format!("{}…{}", &pubkey[..8], &pubkey[pubkey.len() - 4..])
}

/// Turn what a user pasted into a 64-char hex pubkey.
///
/// Accepts an `npub1…` (NIP-19 bech32) or raw hex, because both are in
/// circulation and a user copying a key from another Nostr client will have
/// whichever that client shows. Rejects `nsec1…` explicitly and by name: it is
/// one character away from `npub` in a font people read quickly, and pasting a
/// *secret* key into a "who do you want to message" box is a mistake worth
/// catching loudly rather than failing as "not valid hex".
pub fn pubkey_from_input(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.starts_with("nsec") {
        return Err("that is a private key — you want the npub, not the nsec".into());
    }
    if let Some(rest) = raw.strip_prefix("npub") {
        let _ = rest;
        let (hrp, data) = bech32::decode(raw).map_err(|e| format!("not a valid npub: {e}"))?;
        if hrp.as_str() != "npub" {
            return Err(format!("expected an npub, got '{}'", hrp.as_str()));
        }
        if data.len() != 32 {
            return Err("an npub decodes to 32 bytes".into());
        }
        return Ok(hex::encode(data));
    }
    let bytes = hex::decode(raw).map_err(|_| "not an npub or a hex key".to_string())?;
    if bytes.len() != 32 {
        return Err("a nostr pubkey is 32 bytes (64 hex characters)".into());
    }
    Ok(hex::encode(bytes))
}

/// Last 4 characters of the pubkey — a Discord-style discriminator suffix.
pub fn discriminator(pubkey: &str) -> &str {
    if pubkey.len() <= 4 {
        return pubkey;
    }
    &pubkey[pubkey.len() - 4..]
}

/// A deterministic "color signature" for a pubkey: `n` vivid HSL colors derived
/// by hashing the key. Purely presentational (the row of colored bars on the
/// identity/profile card) — same key always yields the same signature, and no
/// two keys share one. Mirrors the design's `h=(h*31+c)>>>0` seed hash.
pub fn color_signature(pubkey: &str, n: usize) -> Vec<String> {
    let mut h: u32 = 0;
    for b in pubkey.bytes() {
        h = h.wrapping_mul(31).wrapping_add(b as u32);
    }
    (0..n)
        .map(|i| {
            // Advance the hash per bar so hues spread across the wheel.
            h = h.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let hue = h % 360;
            let sat = 62 + (h >> 9) % 24; // 62–86%
            let light = 56 + (h >> 17) % 12; // 56–68%
            let _ = i;
            format!("hsl({hue}, {sat}%, {light}%)")
        })
        .collect()
}

/// The single accent hue for a pubkey (first signature color) — used to tint
/// usernames and profile banners.
pub fn signature_accent(pubkey: &str) -> String {
    color_signature(pubkey, 1)
        .into_iter()
        .next()
        .unwrap_or_else(|| "hsl(30, 70%, 60%)".into())
}

// ---------------------------------------------------------------------------
// NIP-06 key derivation: BIP-32 over secp256k1 along `m/44'/1237'/0'/0/0`.
// Implemented with secp256k1's `add_tweak` (child = parent + IL mod n).
// ---------------------------------------------------------------------------

const HARDENED: u32 = 0x8000_0000;
const NIP06_PATH: [u32; 5] = [44 | HARDENED, 1237 | HARDENED, HARDENED, 0, 0];

fn derive_nip06(seed: &[u8]) -> Result<SecretKey, String> {
    let secp = Secp256k1::new();

    // Master key: HMAC-SHA512("Bitcoin seed", seed).
    let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(b"Bitcoin seed").expect("hmac key length");
    mac.update(seed);
    let master = mac.finalize().into_bytes();
    let mut key = SecretKey::from_slice(&master[..32]).map_err(|e| format!("master key: {e}"))?;
    let mut chain = [0u8; 32];
    chain.copy_from_slice(&master[32..]);

    for &index in &NIP06_PATH {
        let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(&chain).expect("hmac key length");
        if index & HARDENED != 0 {
            // Hardened: 0x00 || ser256(k_par) || ser32(i)
            mac.update(&[0u8]);
            mac.update(&key.secret_bytes());
        } else {
            // Normal: serP(point(k_par)) || ser32(i)  (compressed pubkey)
            let pk = PublicKey::from_secret_key(&secp, &key);
            mac.update(&pk.serialize());
        }
        mac.update(&index.to_be_bytes());
        let i = mac.finalize().into_bytes();
        let il: [u8; 32] = i[..32].try_into().unwrap();
        let tweak =
            Scalar::from_be_bytes(il).map_err(|_| "derived tweak out of range".to_string())?;
        key = key
            .add_tweak(&tweak)
            .map_err(|e| format!("child key: {e}"))?;
        chain.copy_from_slice(&i[32..]);
    }

    Ok(key)
}

#[cfg(test)]
mod pubkey_input_tests {
    use super::*;

    /// Both forms in circulation are accepted, and agree.
    #[test]
    fn npub_and_hex_are_the_same_key() {
        let id = Identity::create("t").expect("identity");
        let from_hex = pubkey_from_input(&id.pubkey).expect("hex");
        let from_npub = pubkey_from_input(&id.npub()).expect("npub");
        assert_eq!(from_hex, id.pubkey);
        assert_eq!(from_npub, id.pubkey);
    }

    /// The mistake worth catching by name. `nsec` and `npub` differ by one
    /// character, and pasting a secret into a "who do you want to message" box
    /// must say so rather than failing as "not valid hex".
    #[test]
    fn an_nsec_is_refused_by_name() {
        let id = Identity::create("t").expect("identity");
        let IdentitySource::Phrase(_) = &id.source else {
            panic!("expected a generated phrase identity")
        };
        let err = pubkey_from_input("nsec1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqsuchhu")
            .expect_err("must refuse");
        assert!(err.contains("private key"), "unhelpful error: {err}");
    }

    /// Garbage, wrong-length hex and empty input all fail rather than
    /// producing a key nobody can be reached at.
    #[test]
    fn nonsense_is_refused() {
        for bad in ["", "hello", "abcd", &"a".repeat(63), &"z".repeat(64)] {
            assert!(pubkey_from_input(bad).is_err(), "{bad:?} should be refused");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secp256k1::XOnlyPublicKey;

    #[test]
    fn create_and_restore_phrase_yield_same_pubkey() {
        let original = Identity::create("alice").unwrap();
        let phrase = match &original.source {
            IdentitySource::Phrase(p) => p.clone(),
            _ => panic!("expected phrase source"),
        };
        let restored = Identity::restore_from_phrase(&phrase, "alice").unwrap();
        assert_eq!(original.pubkey, restored.pubkey);
    }

    #[test]
    fn import_nsec_round_trips() {
        let original = Identity::create("alice").unwrap();
        let nsec = original.export_nsec();
        let imported = Identity::restore_from_private_key(&nsec, "alice").unwrap();
        assert_eq!(original.pubkey, imported.pubkey);
        assert!(matches!(imported.source, IdentitySource::Nsec(_)));
    }

    #[test]
    fn import_hex_secret() {
        let original = Identity::create("alice").unwrap();
        let hex_secret = hex::encode(original.secret.secret_bytes());
        let imported = Identity::restore_from_private_key(&hex_secret, "alice").unwrap();
        assert_eq!(original.pubkey, imported.pubkey);
    }

    #[test]
    fn pubkey_is_64_hex_and_npub() {
        let id = Identity::create("alice").unwrap();
        assert_eq!(id.pubkey.len(), 64);
        assert!(id.pubkey.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(id.npub().starts_with("npub1"));
    }

    #[test]
    fn signing_round_trips() {
        let id = Identity::create("alice").unwrap();
        let msg = b"hello dioxusfun";
        let sig_hex = id.sign_hex(msg);

        let secp = Secp256k1::new();
        let pk_bytes: [u8; 32] = hex::decode(&id.pubkey).unwrap().try_into().unwrap();
        let xonly = XOnlyPublicKey::from_slice(&pk_bytes).unwrap();
        let digest: [u8; 32] = Sha256::digest(msg).into();
        let m = Message::from_digest(digest);
        let sig_bytes: [u8; 64] = hex::decode(&sig_hex).unwrap().try_into().unwrap();
        let sig = secp256k1::schnorr::Signature::from_slice(&sig_bytes).unwrap();
        assert!(secp.verify_schnorr(&sig, &m, &xonly).is_ok());
    }

    #[test]
    fn discriminator_last_four() {
        let pk = "abcdef0123456789";
        assert_eq!(discriminator(pk), "6789");
        assert_eq!(discriminator("abc"), "abc");
    }
}
