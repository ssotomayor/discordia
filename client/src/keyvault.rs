//! Keys at rest. A private key is written as a NIP-49 `ncryptsec` under a
//! random passphrase this machine holds — in the OS keychain where there is
//! one, in a file beside the config otherwise.

use std::path::PathBuf;

use bech32::{Bech32, Hrp};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use zeroize::Zeroizing;

const NCRYPTSEC_VERSION: u8 = 0x02;
const KEY_SECURITY_UNTRACKED: u8 = 0x02;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const ENCODED_LEN: usize = 1 + 1 + SALT_LEN + NONCE_LEN + 1 + 32 + 16;

#[cfg(not(test))]
pub const LOG_N: u8 = 16;
#[cfg(test)]
pub const LOG_N: u8 = 8;

const SERVICE: &str = "com.discordia.app";
const ACCOUNT: &str = "identity-vault";
const BACKEND_MARKER: &str = "vault.backend";
const KEY_FILE: &str = "vault.key";
pub const ENV_BACKEND: &str = "DIOXUSFUN_VAULT";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Keychain,
    File,
}

impl Backend {
    fn marker(self) -> &'static str {
        match self {
            Backend::Keychain => "keychain",
            Backend::File => "file",
        }
    }

    fn from_marker(s: &str) -> Option<Self> {
        match s.trim() {
            "keychain" => Some(Backend::Keychain),
            "file" => Some(Backend::File),
            _ => None,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Backend::Keychain if cfg!(target_os = "macos") => "the macOS Keychain",
            Backend::Keychain if cfg!(target_os = "windows") => "the Windows Credential Manager",
            Backend::Keychain => "the desktop's Secret Service",
            Backend::File => "a file in the config folder",
        }
    }
}

fn marker_path() -> PathBuf {
    crate::identity::config_dir().join(BACKEND_MARKER)
}

fn key_file_path() -> PathBuf {
    crate::identity::config_dir().join(KEY_FILE)
}

/// Where the passphrase lives. Decided once and remembered, because a backend
/// that quietly changed between runs would leave every key on disk unreadable.
pub fn backend() -> Backend {
    if let Some(forced) = std::env::var(ENV_BACKEND)
        .ok()
        .and_then(|v| Backend::from_marker(&v))
    {
        return forced;
    }
    if cfg!(test) {
        return Backend::File;
    }
    if let Some(chosen) = std::fs::read_to_string(marker_path())
        .ok()
        .and_then(|s| Backend::from_marker(&s))
    {
        return chosen;
    }
    let chosen = if keychain_usable() {
        Backend::Keychain
    } else {
        Backend::File
    };
    if let Err(e) = crate::identity::write_private(&marker_path(), chosen.marker()) {
        tracing::warn!(error = %e, "could not remember the vault backend");
    }
    chosen
}

fn keychain_usable() -> bool {
    let Ok(entry) = keyring::Entry::new(SERVICE, ACCOUNT) else {
        return false;
    };
    match entry.get_password() {
        Ok(_) => true,
        Err(keyring::Error::NoEntry) => match entry.set_password(&fresh_passphrase()) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "keychain refused a write; keys fall back to a file");
                false
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "keychain unavailable; keys fall back to a file");
            false
        }
    }
}

fn fresh_passphrase() -> Zeroizing<String> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(bytes.as_mut());
    Zeroizing::new(hex::encode(bytes.as_ref()))
}

/// The machine passphrase, created on first use.
pub fn passphrase() -> Result<Zeroizing<String>, String> {
    match backend() {
        Backend::Keychain => {
            let entry =
                keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| format!("keychain: {e}"))?;
            match entry.get_password() {
                Ok(p) => Ok(Zeroizing::new(p)),
                Err(keyring::Error::NoEntry) => {
                    let p = fresh_passphrase();
                    entry
                        .set_password(&p)
                        .map_err(|e| format!("keychain would not store the vault key: {e}"))?;
                    Ok(p)
                }
                Err(e) => Err(format!(
                    "the vault key is in {} and it could not be opened: {e}",
                    Backend::Keychain.describe()
                )),
            }
        }
        Backend::File => {
            let path = key_file_path();
            match std::fs::read_to_string(&path) {
                Ok(p) => Ok(Zeroizing::new(p.trim().to_string())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let p = fresh_passphrase();
                    crate::identity::write_private(&path, &p)?;
                    Ok(p)
                }
                Err(e) => Err(format!("read vault key: {e}")),
            }
        }
    }
}

fn derive(passphrase: &str, salt: &[u8], log_n: u8) -> Result<Zeroizing<[u8; 32]>, String> {
    // The passphrase is hex we generated, so NIP-49's NFKC step is the identity.
    let params = scrypt::Params::new(log_n, 8, 1, 32).map_err(|e| format!("scrypt: {e}"))?;
    let mut key = Zeroizing::new([0u8; 32]);
    scrypt::scrypt(passphrase.as_bytes(), salt, &params, key.as_mut())
        .map_err(|e| format!("scrypt: {e}"))?;
    Ok(key)
}

pub fn encrypt(secret: &[u8; 32], passphrase: &str, log_n: u8) -> Result<String, String> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let key = derive(passphrase, &salt, log_n)?;
    let cipher = XChaCha20Poly1305::new(key.as_ref().into());
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: secret,
                aad: &[KEY_SECURITY_UNTRACKED],
            },
        )
        .map_err(|_| "encrypt key".to_string())?;

    let mut out = Vec::with_capacity(ENCODED_LEN);
    out.push(NCRYPTSEC_VERSION);
    out.push(log_n);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.push(KEY_SECURITY_UNTRACKED);
    out.extend_from_slice(&ciphertext);
    let hrp = Hrp::parse("ncryptsec").expect("valid hrp");
    bech32::encode::<Bech32>(hrp, &out).map_err(|e| format!("encode ncryptsec: {e}"))
}

pub fn decrypt(ncryptsec: &str, passphrase: &str) -> Result<Zeroizing<[u8; 32]>, String> {
    let (hrp, data) = bech32::decode(ncryptsec.trim()).map_err(|e| format!("ncryptsec: {e}"))?;
    if hrp.as_str() != "ncryptsec" {
        return Err(format!("expected ncryptsec, got '{}'", hrp.as_str()));
    }
    if data.len() != ENCODED_LEN || data[0] != NCRYPTSEC_VERSION {
        return Err("ncryptsec has an unknown shape or version".into());
    }
    let log_n = data[1];
    let salt = &data[2..2 + SALT_LEN];
    let nonce = &data[2 + SALT_LEN..2 + SALT_LEN + NONCE_LEN];
    let security = data[2 + SALT_LEN + NONCE_LEN];
    let ciphertext = &data[2 + SALT_LEN + NONCE_LEN + 1..];

    let key = derive(passphrase, salt, log_n)?;
    let cipher = XChaCha20Poly1305::new(key.as_ref().into());
    let plain = cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: &[security],
            },
        )
        .map_err(|_| {
            "this key was locked with a vault passphrase this machine no longer has; \
             import it again from its recovery phrase or nsec"
                .to_string()
        })?;
    let mut plain = Zeroizing::new(plain);
    let mut out = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(&plain);
    plain.clear();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_survives_the_round_trip_and_nothing_else_opens_it() {
        let secret = [7u8; 32];
        let enc = encrypt(&secret, "correct horse", LOG_N).expect("encrypt");
        assert!(enc.starts_with("ncryptsec1"), "{enc}");
        assert_eq!(*decrypt(&enc, "correct horse").expect("decrypt"), secret);
        assert!(decrypt(&enc, "wrong horse").is_err());

        let mut tampered = enc.clone().into_bytes();
        let i = tampered.len() - 10;
        tampered[i] = if tampered[i] == b'q' { b'p' } else { b'q' };
        assert!(decrypt(std::str::from_utf8(&tampered).unwrap(), "correct horse").is_err());
    }

    #[test]
    fn two_encryptions_of_one_key_differ() {
        let secret = [9u8; 32];
        let a = encrypt(&secret, "p", LOG_N).unwrap();
        let b = encrypt(&secret, "p", LOG_N).unwrap();
        assert_ne!(a, b, "salt and nonce are fresh each time");
    }
}
