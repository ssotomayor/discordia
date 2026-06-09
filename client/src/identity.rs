//! Cryptographic identity.
//!
//! Each user has an Ed25519 keypair derived from a BIP39 mnemonic via the
//! Solana SLIP-0010 path `m/44'/501'/0'/0'`. The public key (base58 encoded)
//! is the universal user identifier — same identity across every server.
//!
//! The identity file lives at the OS-appropriate config dir
//! (`~/.config/dioxusfun/identity.json` on Linux,
//! `~/Library/Application Support/dioxusfun/identity.json` on macOS, etc.).
//! It stores the seed phrase in plaintext for now — fine for friends-only
//! use, would move into OS keychain for production.

use std::path::PathBuf;

use bip39::{Language, Mnemonic};
use ed25519_dalek::{Signer, SigningKey};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha512;

const FILE_VERSION: u32 = 1;

#[derive(Clone)]
pub struct Identity {
    pub pubkey: String,
    pub display_name: String,
    pub seed_phrase: String,
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
            .field("seed_phrase", &"<redacted>")
            .finish()
    }
}

impl Identity {
    /// Generate a fresh keypair with a brand-new BIP39 phrase (12 words / 128
    /// bits entropy).
    pub fn create(display_name: impl Into<String>) -> Result<Self, String> {
        use rand::RngCore;
        let mut entropy = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut entropy);
        let mnemonic = Mnemonic::from_entropy_in(Language::English, &entropy)
            .map_err(|e: bip39::Error| e.to_string())?;
        Ok(Self::from_mnemonic(mnemonic, display_name.into()))
    }

    /// Restore from a 12 (or 24) word BIP39 phrase.
    pub fn restore(
        seed_phrase: impl AsRef<str>,
        display_name: impl Into<String>,
    ) -> Result<Self, String> {
        let mnemonic =
            Mnemonic::parse_in_normalized(Language::English, seed_phrase.as_ref().trim())
                .map_err(|e: bip39::Error| format!("invalid recovery phrase: {e}"))?;
        Ok(Self::from_mnemonic(mnemonic, display_name.into()))
    }

    fn from_mnemonic(mnemonic: Mnemonic, display_name: String) -> Self {
        let seed: [u8; 64] = mnemonic.to_seed("");
        let signing_key = derive_solana(&seed);
        let pubkey = bs58::encode(signing_key.verifying_key().as_bytes()).into_string();
        Self {
            pubkey,
            display_name,
            seed_phrase: mnemonic.to_string(),
            signing_key,
        }
    }

    /// Sign an arbitrary message. Used by the gateway handshake to prove
    /// ownership of the pubkey (sign the server-issued nonce).
    pub fn sign_base58(&self, message: &[u8]) -> String {
        let sig = self.signing_key.sign(message);
        bs58::encode(sig.to_bytes()).into_string()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = identity_path();
        let parent = path
            .parent()
            .ok_or_else(|| "identity path has no parent".to_string())?;
        std::fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
        let stored = Stored {
            version: FILE_VERSION,
            display_name: self.display_name.clone(),
            pubkey: self.pubkey.clone(),
            seed_phrase: self.seed_phrase.clone(),
        };
        let content = serde_json::to_string_pretty(&stored)
            .map_err(|e| format!("serialize identity: {e}"))?;
        std::fs::write(&path, content).map_err(|e| format!("write identity: {e}"))?;
        // chmod 600 on Unix so other local users can't read the seed.
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
        let stored: Stored = serde_json::from_str(&content)
            .map_err(|e| format!("parse identity: {e}"))?;
        if stored.version != FILE_VERSION {
            return Err(format!(
                "unknown identity file version {}; expected {}",
                stored.version, FILE_VERSION
            ));
        }
        let identity = Self::restore(stored.seed_phrase, stored.display_name)?;
        Ok(Some(identity))
    }

    /// Returns the path used by [`Identity::save`] / [`Identity::load`].
    #[allow(dead_code)] // exposed for future debug / "show me where my key is" UI
    pub fn file_path() -> PathBuf {
        identity_path()
    }

    /// Wipe the on-disk identity. Used by "Sign out" in the UI.
    pub fn delete_file() -> Result<(), String> {
        let path = identity_path();
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("remove identity: {e}"))?;
        }
        Ok(())
    }

    /// Update the display name and persist.
    #[allow(dead_code)] // exposed for the future "edit identity" drawer
    pub fn set_display_name(&mut self, name: impl Into<String>) -> Result<(), String> {
        self.display_name = name.into();
        self.save()
    }

    /// Convenience: short form of the pubkey for compact UI like "7xKX…M3qb".
    pub fn truncated_pubkey(&self) -> String {
        truncate_pubkey(&self.pubkey)
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    display_name: String,
    pubkey: String,
    seed_phrase: String,
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

// ---------------------------------------------------------------------------
// SLIP-0010 ed25519 derivation along Solana's standard path m/44'/501'/0'/0'.
// All segments are hardened (high bit set).
// ---------------------------------------------------------------------------

const SOLANA_PATH: [u32; 4] = [
    44 | 0x8000_0000,
    501 | 0x8000_0000,
    0 | 0x8000_0000,
    0 | 0x8000_0000,
];

fn derive_solana(seed: &[u8]) -> SigningKey {
    // Master key.
    let mut mac =
        <Hmac<Sha512> as Mac>::new_from_slice(b"ed25519 seed").expect("hmac key length");
    mac.update(seed);
    let master = mac.finalize().into_bytes();

    let mut key = [0u8; 32];
    let mut chain = [0u8; 32];
    key.copy_from_slice(&master[..32]);
    chain.copy_from_slice(&master[32..]);

    // Walk the path, each segment hardened.
    for &index in &SOLANA_PATH {
        let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(&chain).expect("hmac key length");
        mac.update(&[0]); // hardened prefix
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
    fn create_and_restore_yield_same_pubkey() {
        let original = Identity::create("alice").unwrap();
        let restored = Identity::restore(&original.seed_phrase, "alice").unwrap();
        assert_eq!(original.pubkey, restored.pubkey);
    }

    #[test]
    fn pubkey_is_44_chars_base58() {
        let id = Identity::create("alice").unwrap();
        assert!(id.pubkey.len() >= 32 && id.pubkey.len() <= 44);
        // Solana addresses don't contain 0, O, I, or l.
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
}
