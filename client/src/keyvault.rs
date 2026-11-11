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
pub const ENV_BACKEND: &str = "DISCORDIA_VAULT";

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
            Backend::Keychain if cfg!(target_os = "ios") => "the iOS Keychain",
            Backend::Keychain if cfg!(target_os = "android") => "Android Keystore",
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
#[cfg(target_os = "android")]
pub fn backend() -> Result<Backend, String> {
    Ok(Backend::Keychain)
}

#[cfg(not(target_os = "android"))]
pub fn backend() -> Result<Backend, String> {
    let marker = std::fs::read_to_string(marker_path())
        .ok()
        .and_then(|s| Backend::from_marker(&s));
    let forced = std::env::var(ENV_BACKEND)
        .ok()
        .and_then(|v| Backend::from_marker(&v));
    if let Some(chosen) = settled(forced, marker)? {
        return Ok(chosen);
    }
    if cfg!(test) {
        return Ok(Backend::File);
    }
    if let Some(chosen) = marker {
        return Ok(chosen);
    }
    // A key file without its marker is still the passphrase in use; probing the
    // keychain would mint a rival one.
    if key_file_path().exists() {
        return Ok(Backend::File);
    }
    Ok(if keychain_usable()? {
        Backend::Keychain
    } else {
        Backend::File
    })
}

#[cfg(not(target_os = "android"))]
fn settled(forced: Option<Backend>, marker: Option<Backend>) -> Result<Option<Backend>, String> {
    match (forced, marker) {
        (Some(forced), Some(marker)) if forced != marker => Err(format!(
            "{ENV_BACKEND}={} disagrees with vault.backend ({}), which locked this machine's identities. Unset it or make them match.",
            forced.marker(),
            marker.marker()
        )),
        (forced, _) => Ok(forced),
    }
}

#[cfg(not(target_os = "android"))]
fn keychain_usable() -> Result<bool, String> {
    let Ok(entry) = keyring::Entry::new(SERVICE, ACCOUNT) else {
        return Ok(false);
    };
    match entry.get_password() {
        Ok(_) => Ok(true),
        // Usable, but empty where keys already exist: `passphrase` refuses, never mints.
        Err(keyring::Error::NoEntry) if vault_state_exists()? => Ok(true),
        Err(keyring::Error::NoEntry) => match entry.set_password(&fresh_passphrase()) {
            Ok(()) => Ok(true),
            Err(e) => {
                tracing::warn!(error = %e, "keychain refused a write; keys fall back to a file");
                Ok(false)
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "keychain unavailable; keys fall back to a file");
            Ok(false)
        }
    }
}

/// Anything already locked under a passphrase. A new one would orphan it for
/// good, and the old one could never be put back.
#[cfg(not(target_os = "android"))]
fn vault_state_exists() -> Result<bool, String> {
    Ok(marker_path().exists()
        || key_file_path().exists()
        || crate::identity::encrypted_keys_exist()?)
}

#[cfg(not(target_os = "android"))]
fn missing(backend: Backend) -> String {
    match backend {
        Backend::Keychain => MISSING_FROM_KEYCHAIN,
        Backend::File => MISSING_FROM_FILE,
    }
    .to_string()
}

#[cfg(not(target_os = "android"))]
const MISSING_FROM_KEYCHAIN: &str = "The vault key for this machine's identities is missing from the system keychain. Nothing was replaced: restore it, or move the identities folder and vault.backend out of the config folder to start over.";
#[cfg(not(target_os = "android"))]
const MISSING_FROM_FILE: &str = "The vault key for this machine's identities (vault.key) is missing from the config folder. Nothing was replaced: restore it, or move the identities folder and vault.backend out of the config folder to start over.";

fn fresh_passphrase() -> Zeroizing<String> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(bytes.as_mut());
    Zeroizing::new(hex::encode(bytes.as_ref()))
}

/// The machine passphrase, created only on a first run: once anything is
/// locked under one, a missing passphrase is an error, never a fresh one.
#[cfg(target_os = "android")]
pub fn passphrase() -> Result<Zeroizing<String>, String> {
    crate::android::vault_passphrase()
}

#[cfg(not(target_os = "android"))]
pub fn passphrase() -> Result<Zeroizing<String>, String> {
    let backend = backend()?;
    let passphrase = match backend {
        Backend::Keychain => {
            let entry =
                keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| format!("keychain: {e}"))?;
            match entry.get_password() {
                Ok(p) => Zeroizing::new(p),
                Err(keyring::Error::NoEntry) if vault_state_exists()? => {
                    return Err(missing(backend));
                }
                Err(keyring::Error::NoEntry) => {
                    let p = fresh_passphrase();
                    entry
                        .set_password(&p)
                        .map_err(|e| format!("keychain would not store the vault key: {e}"))?;
                    p
                }
                Err(e) => {
                    return Err(format!(
                        "the vault key is in {} and it could not be opened: {e}",
                        Backend::Keychain.describe()
                    ));
                }
            }
        }
        Backend::File => {
            let path = key_file_path();
            match std::fs::read_to_string(&path) {
                Ok(p) => Zeroizing::new(p.trim().to_string()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if vault_state_exists()? {
                        return Err(missing(backend));
                    }
                    let p = fresh_passphrase();
                    crate::identity::write_private(&path, &p)?;
                    p
                }
                Err(e) => return Err(format!("read vault key: {e}")),
            }
        }
    };
    // Written only once a passphrase is in hand, so a first run that failed
    // halfway is not mistaken next time for a vault that lost its key.
    if !marker_path().exists()
        && let Err(e) = crate::identity::write_private(&marker_path(), backend.marker())
    {
        tracing::warn!(error = %e, "could not remember the vault backend");
    }
    Ok(passphrase)
}

#[cfg(test)]
thread_local! {
    static DERIVATIONS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn derivation_count() -> u32 {
    DERIVATIONS.with(|count| count.get())
}

fn derive(passphrase: &str, salt: &[u8], log_n: u8) -> Result<Zeroizing<[u8; 32]>, String> {
    #[cfg(test)]
    DERIVATIONS.with(|count| count.set(count.get() + 1));
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

const SEALED_VERSION: u8 = 0x01;
// The project's old name, kept: a new AAD would leave every sealed phrase unreadable.
const SEALED_AAD: &[u8] = b"dioxusfun/sealed/v1";
const SEALED_HEAD: usize = 1 + 1 + SALT_LEN + NONCE_LEN;

/// Same passphrase, same cipher, any length. `encrypt` stays at 32 bytes so a
/// key remains an `ncryptsec` another wallet can read; nothing else reads this.
pub fn seal(plain: &[u8], passphrase: &str, log_n: u8) -> Result<String, String> {
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
                msg: plain,
                aad: SEALED_AAD,
            },
        )
        .map_err(|_| "seal".to_string())?;
    let mut out = Vec::with_capacity(SEALED_HEAD + ciphertext.len());
    out.push(SEALED_VERSION);
    out.push(log_n);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(hex::encode(out))
}

pub fn unseal(sealed: &str, passphrase: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let data = hex::decode(sealed.trim()).map_err(|e| format!("sealed data: {e}"))?;
    if data.len() < SEALED_HEAD + 16 || data[0] != SEALED_VERSION {
        return Err("sealed data has an unknown shape or version".into());
    }
    let log_n = data[1];
    let salt = &data[2..2 + SALT_LEN];
    let nonce = &data[2 + SALT_LEN..SEALED_HEAD];
    let ciphertext = &data[SEALED_HEAD..];
    let key = derive(passphrase, salt, log_n)?;
    let cipher = XChaCha20Poly1305::new(key.as_ref().into());
    cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: SEALED_AAD,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| "sealed with a vault passphrase this machine no longer has".to_string())
}

/// The scrypt cost a blob names for itself. Opening one allocates 2^cost KiB,
/// so a blob that came from elsewhere is checked before it is opened.
pub fn cost(blob: &str) -> Option<u8> {
    let blob = blob.trim();
    match bech32::decode(blob) {
        Ok((_, data)) => data.get(1).copied(),
        Err(_) => hex::decode(blob).ok()?.get(1).copied(),
    }
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
    fn a_first_run_makes_the_passphrase_and_later_runs_reuse_it() {
        let dir = crate::identity::test_config_dir();
        let first = passphrase().expect("first run");
        assert_eq!(*passphrase().expect("second run"), *first);
        assert_eq!(
            std::fs::read_to_string(dir.join(BACKEND_MARKER)).unwrap(),
            "file"
        );
    }

    #[test]
    fn a_lost_vault_key_is_refused_rather_than_replaced() {
        let dir = crate::identity::test_config_dir();
        let id = crate::identity::Identity::restore_from_private_key("44".repeat(32), "kept")
            .expect("identity");
        id.save().expect("save");
        let original = std::fs::read_to_string(dir.join(KEY_FILE)).unwrap();
        std::fs::remove_file(dir.join(KEY_FILE)).unwrap();

        assert_eq!(passphrase().unwrap_err(), MISSING_FROM_FILE);
        assert!(!dir.join(KEY_FILE).exists(), "no rival passphrase minted");

        std::fs::write(dir.join(KEY_FILE), original).unwrap();
        let back = crate::identity::Identity::load().unwrap().unwrap();
        assert_eq!(back.pubkey, id.pubkey);
    }

    #[test]
    fn a_marker_without_its_passphrase_is_refused() {
        let dir = crate::identity::test_config_dir();
        std::fs::write(dir.join(BACKEND_MARKER), "file").unwrap();
        assert_eq!(passphrase().unwrap_err(), MISSING_FROM_FILE);
    }

    #[test]
    fn an_env_backend_must_agree_with_the_marker() {
        use Backend::{File, Keychain};
        assert!(settled(Some(File), Some(Keychain)).is_err());
        assert_eq!(settled(Some(File), Some(File)), Ok(Some(File)));
        assert_eq!(settled(Some(Keychain), None), Ok(Some(Keychain)));
        assert_eq!(settled(None, Some(Keychain)), Ok(None));
    }

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
    fn sealed_bytes_of_any_length_round_trip_and_do_not_pass_as_a_key() {
        let entropy = [9u8; 16];
        let sealed = seal(&entropy, "correct horse", LOG_N).expect("seal");
        assert_eq!(
            **unseal(&sealed, "correct horse").expect("unseal"),
            entropy[..]
        );
        assert!(unseal(&sealed, "wrong horse").is_err());
        assert!(decrypt(&sealed, "correct horse").is_err());
        let key = encrypt(&[7u8; 32], "correct horse", LOG_N).expect("encrypt");
        assert!(unseal(&key, "correct horse").is_err());
    }

    #[test]
    fn cost_reads_the_scrypt_exponent_of_either_kind() {
        let key = encrypt(&[7u8; 32], "p", LOG_N).unwrap();
        let sealed = seal(&[9u8; 16], "p", LOG_N).unwrap();
        assert_eq!(cost(&key), Some(LOG_N));
        assert_eq!(cost(&sealed), Some(LOG_N));
        assert_eq!(cost("not a blob"), None);
    }

    #[test]
    fn two_encryptions_of_one_key_differ() {
        let secret = [9u8; 32];
        let a = encrypt(&secret, "p", LOG_N).unwrap();
        let b = encrypt(&secret, "p", LOG_N).unwrap();
        assert_ne!(a, b, "salt and nonce are fresh each time");
    }
}
