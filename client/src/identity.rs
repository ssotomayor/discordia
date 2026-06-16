//! Cryptographic identity.
//!
//! Each user has an Ed25519 keypair. There are two ways to create one:
//!
//! 1. **BIP39 seed phrase** — generated at first launch, derived along the
//!    Solana SLIP-0010 path `m/44'/501'/0'/0'`. The 12-word phrase is the
//!    recovery format and is interchangeable with Phantom/Solflare.
//! 2. **Raw private key import** — paste a base58-encoded 32-byte secret
//!    (or a 64-byte `secret||pubkey` keypair, the format Phantom exports).
//!    No seed phrase is available for keys imported this way.
//!
//! Persisted to the OS-appropriate config dir as `dioxusfun/identity.json`:
//!
//! - macOS:   `~/Library/Application Support/dioxusfun/identity.json`
//! - Linux:   `~/.config/dioxusfun/identity.json`
//! - Windows: `%APPDATA%\dioxusfun\identity.json`
//!
//! Mode `0600` on Unix. Plaintext key — fine for friends-only; move into
//! OS keychain for anything serious.

use std::path::PathBuf;

use bip39::{Language, Mnemonic};
use ed25519_dalek::{Signer, SigningKey};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha512;

const FILE_VERSION: u32 = 1;

/// Where the identity came from. Determines what kind of recovery info we
/// can show the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentitySource {
    /// 12-word BIP39 mnemonic. Compatible with Phantom/Solflare.
    Phrase(String),
    /// Base58-encoded 32-byte Ed25519 secret. No seed phrase available.
    PrivateKey(String),
}

impl IdentitySource {
    /// Used by the future identity drawer to decide between showing the
    /// 12-word recovery phrase or the raw private key.
    #[allow(dead_code)]
    pub fn is_phrase(&self) -> bool {
        matches!(self, IdentitySource::Phrase(_))
    }
}

#[derive(Clone)]
pub struct Identity {
    pub pubkey: String,
    pub display_name: String,
    pub source: IdentitySource,
    signing_key: SigningKey,
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
        Ok(Self::from_mnemonic(mnemonic, display_name.into()))
    }

    /// Restore from a 12- or 24-word BIP39 phrase.
    pub fn restore_from_phrase(
        seed_phrase: impl AsRef<str>,
        display_name: impl Into<String>,
    ) -> Result<Self, String> {
        let mnemonic =
            Mnemonic::parse_in_normalized(Language::English, seed_phrase.as_ref().trim())
                .map_err(|e: bip39::Error| format!("invalid recovery phrase: {e}"))?;
        Ok(Self::from_mnemonic(mnemonic, display_name.into()))
    }

    /// Import from a raw private key (base58, 32 or 64 bytes). 64-byte input
    /// is Phantom/Solflare's standard export — `secret_key || pubkey`. We
    /// take the first 32 bytes (the secret) and derive the pubkey from it.
    pub fn restore_from_private_key(
        private_key_b58: impl AsRef<str>,
        display_name: impl Into<String>,
    ) -> Result<Self, String> {
        let raw = bs58::decode(private_key_b58.as_ref().trim())
            .into_vec()
            .map_err(|e| format!("private key is not valid base58: {e}"))?;
        let secret: [u8; 32] = match raw.len() {
            32 => raw.try_into().unwrap(),
            64 => raw[..32].try_into().unwrap(),
            n => {
                return Err(format!(
                    "private key must be 32 or 64 bytes (got {n}); export from Phantom is 64"
                ));
            }
        };
        let signing_key = SigningKey::from_bytes(&secret);
        let pubkey = bs58::encode(signing_key.verifying_key().as_bytes()).into_string();
        let stored_secret = bs58::encode(secret).into_string();
        Ok(Self {
            pubkey,
            display_name: display_name.into(),
            source: IdentitySource::PrivateKey(stored_secret),
            signing_key,
        })
    }

    fn from_mnemonic(mnemonic: Mnemonic, display_name: String) -> Self {
        let seed: [u8; 64] = mnemonic.to_seed("");
        let signing_key = derive_solana(&seed);
        let pubkey = bs58::encode(signing_key.verifying_key().as_bytes()).into_string();
        let phrase = mnemonic.to_string();
        Self {
            pubkey,
            display_name,
            source: IdentitySource::Phrase(phrase),
            signing_key,
        }
    }

    /// Sign an arbitrary message; returns base58.
    pub fn sign_base58(&self, message: &[u8]) -> String {
        let sig = self.signing_key.sign(message);
        bs58::encode(sig.to_bytes()).into_string()
    }

    /// Clone of the underlying signing key — used by the in-app wallet to
    /// sign Solana transactions. The clone is a separate SigningKey value
    /// from the same secret bytes, not a reference, so callers can move it
    /// into async tasks.
    pub fn signing_key_clone(&self) -> SigningKey {
        SigningKey::from_bytes(&self.signing_key.to_bytes())
    }

    /// Base58 of the raw 32-byte secret key. Used by tests + the future
    /// identity drawer to let users export-as-private-key (e.g. to import
    /// into Phantom).
    #[allow(dead_code)]
    pub fn export_private_key_b58(&self) -> String {
        bs58::encode(self.signing_key.to_bytes()).into_string()
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
            private_key: None,
        };
        match &self.source {
            IdentitySource::Phrase(p) => stored.seed_phrase = Some(p.clone()),
            IdentitySource::PrivateKey(k) => stored.private_key = Some(k.clone()),
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
        // Prefer seed_phrase over private_key when both are somehow present
        // (e.g. user edited the file by hand) — phrase is richer.
        let identity = if let Some(phrase) = stored.seed_phrase {
            Self::restore_from_phrase(&phrase, stored.display_name)?
        } else if let Some(pk) = stored.private_key {
            Self::restore_from_private_key(&pk, stored.display_name)?
        } else {
            return Err("identity file has neither seed_phrase nor private_key".into());
        };
        Ok(Some(identity))
    }

    /// Path the identity file lives at — used by tests + UI affordances.
    #[allow(dead_code)]
    pub fn file_path() -> PathBuf {
        identity_path()
    }

    /// File-system path as a UTF-8 string for display purposes.
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

    #[allow(dead_code)] // exposed for the future "edit identity" drawer
    pub fn set_display_name(&mut self, name: impl Into<String>) -> Result<(), String> {
        self.display_name = name.into();
        self.save()
    }

    pub fn truncated_pubkey(&self) -> String {
        truncate_pubkey(&self.pubkey)
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
    private_key: Option<String>,
}

fn identity_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("dioxusfun")
        .join("identity.json")
}

pub fn truncate_pubkey(pubkey: &str) -> String {
    if pubkey.len() <= 10 {
        return pubkey.to_string();
    }
    format!("{}…{}", &pubkey[..4], &pubkey[pubkey.len() - 4..])
}

/// Last 4 base58 characters of a pubkey — used as a Discord-style
/// discriminator suffix on display names. With ~58 possible characters
/// per slot, collisions in last-4 require ~330k users sharing a single
/// name before > 50% chance, which is plenty for a self-hosted setup.
pub fn discriminator(pubkey: &str) -> &str {
    if pubkey.len() <= 4 {
        return pubkey;
    }
    &pubkey[pubkey.len() - 4..]
}

/// Format a display name as `alice#7xK3` for textual contexts where
/// inline styling isn't available (logs, tooltips, copy-to-clipboard).
/// In rsx! we usually compose the parts directly so the discriminator
/// can be a dimmer color.
#[allow(dead_code)]
pub fn name_with_tag(name: &str, pubkey: &str) -> String {
    format!("{name}#{}", discriminator(pubkey))
}

// ---------------------------------------------------------------------------
// SLIP-0010 ed25519 derivation along Solana's standard path m/44'/501'/0'/0'.
// ---------------------------------------------------------------------------

const SOLANA_PATH: [u32; 4] = [
    44 | 0x8000_0000,
    501 | 0x8000_0000,
    0 | 0x8000_0000,
    0 | 0x8000_0000,
];

fn derive_solana(seed: &[u8]) -> SigningKey {
    let mut mac =
        <Hmac<Sha512> as Mac>::new_from_slice(b"ed25519 seed").expect("hmac key length");
    mac.update(seed);
    let master = mac.finalize().into_bytes();

    let mut key = [0u8; 32];
    let mut chain = [0u8; 32];
    key.copy_from_slice(&master[..32]);
    chain.copy_from_slice(&master[32..]);

    for &index in &SOLANA_PATH {
        let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(&chain).expect("hmac key length");
        mac.update(&[0]);
        mac.update(&key);
        mac.update(&index.to_be_bytes());
        let result = mac.finalize().into_bytes();
        key.copy_from_slice(&result[..32]);
        chain.copy_from_slice(&result[32..]);
    }

    SigningKey::from_bytes(&key)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn import_32_byte_private_key() {
        let original = Identity::create("alice").unwrap();
        let pk_b58 = original.export_private_key_b58();
        let imported = Identity::restore_from_private_key(&pk_b58, "alice").unwrap();
        assert_eq!(original.pubkey, imported.pubkey);
        assert!(matches!(imported.source, IdentitySource::PrivateKey(_)));
    }

    #[test]
    fn import_64_byte_keypair_phantom_style() {
        let original = Identity::create("alice").unwrap();
        // Phantom export: 32 bytes secret + 32 bytes pubkey, both base58 of
        // the 64-byte concat.
        let secret_bytes = bs58::decode(original.export_private_key_b58()).into_vec().unwrap();
        let pubkey_bytes = bs58::decode(&original.pubkey).into_vec().unwrap();
        let mut combined = secret_bytes;
        combined.extend(pubkey_bytes);
        let phantom_format = bs58::encode(&combined).into_string();
        let imported = Identity::restore_from_private_key(&phantom_format, "alice").unwrap();
        assert_eq!(original.pubkey, imported.pubkey);
    }

    #[test]
    fn name_with_tag_appends_last_four() {
        let pk = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";
        assert_eq!(discriminator(pk), "AWWM");
        assert_eq!(name_with_tag("alice", pk), "alice#AWWM");
        // Short input passes through.
        assert_eq!(discriminator("abc"), "abc");
    }

    #[test]
    fn pubkey_is_solana_format_base58() {
        let id = Identity::create("alice").unwrap();
        assert!(id.pubkey.len() >= 32 && id.pubkey.len() <= 44);
        assert!(!id.pubkey.contains(['0', 'O', 'I', 'l']));
    }

    #[test]
    fn signing_round_trips() {
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let id = Identity::create("alice").unwrap();
        let msg = b"hello dioxusfun";
        let sig_b58 = id.sign_base58(msg);

        let pubkey_bytes: [u8; 32] = bs58::decode(&id.pubkey)
            .into_vec()
            .unwrap()
            .try_into()
            .unwrap();
        let vk = VerifyingKey::from_bytes(&pubkey_bytes).unwrap();
        let sig_bytes: [u8; 64] = bs58::decode(&sig_b58).into_vec().unwrap().try_into().unwrap();
        let sig = Signature::from_bytes(&sig_bytes);
        assert!(vk.verify(msg, &sig).is_ok());
    }

    #[test]
    fn truncate_pubkey_format() {
        let s = "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU";
        assert_eq!(truncate_pubkey(s), "7xKX…gAsU");
    }

    #[test]
    fn rejects_wrong_length_private_key() {
        // 16 random bytes — wrong length.
        let bad = bs58::encode([0u8; 16]).into_string();
        assert!(Identity::restore_from_private_key(&bad, "alice").is_err());
    }
}
