//! The file a key becomes in someone's Google Drive. Without a password it is
//! the key in the clear, by the person's choice; with one it is NIP-49.

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

use crate::identity::{Identity, IdentitySource};

const FORMAT: &str = "discordia-identity-backup";
const VERSION: u32 = 1;
const NAME_PREFIX: &str = "identity-";
const NAME_SUFFIX: &str = ".json";
pub const MIN_PASSWORD_CHARS: usize = 8;
pub const MAX_BYTES: usize = 16 * 1024;
// 2^18 KiB is 256 MiB of scrypt. Ours use `keyvault::LOG_N`; a file someone else
// wrote could name 2^40.
const MAX_LOG_N: u8 = 18;

#[derive(Serialize, Deserialize)]
struct File {
    format: String,
    version: u32,
    pubkey: String,
    display_name: String,
    #[serde(flatten)]
    secret: Secret,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "protection", rename_all = "kebab-case")]
enum Secret {
    None {
        nsec: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phrase: Option<String>,
    },
    Password {
        ncryptsec: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entropy: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub pubkey: String,
    pub display_name: String,
    pub protected: bool,
}

pub fn file_name(pubkey: &str) -> String {
    format!("{NAME_PREFIX}{pubkey}{NAME_SUFFIX}")
}

pub fn is_backup_name(name: &str) -> bool {
    name.strip_prefix(NAME_PREFIX)
        .and_then(|rest| rest.strip_suffix(NAME_SUFFIX))
        .is_some_and(crate::protocol::is_canonical_pubkey)
}

// NIP-49 asks for NFKC, and macOS input can hand us the decomposed spelling of
// a password typed precomposed on Windows.
fn normalize(password: &str) -> Zeroizing<String> {
    Zeroizing::new(password.nfkc().collect())
}

pub fn password_acceptable(password: &str) -> bool {
    password.chars().count() >= MIN_PASSWORD_CHARS
}

pub fn seal(identity: &Identity, password: Option<&str>) -> Result<Zeroizing<String>, String> {
    let secret = match password {
        None => Secret::None {
            nsec: identity.export_nsec(),
            phrase: match &identity.source {
                IdentitySource::Phrase(phrase) => Some(phrase.clone()),
                IdentitySource::Nsec(_) => None,
            },
        },
        Some(password) => {
            if !password_acceptable(password) {
                return Err(format!(
                    "a backup password needs at least {MIN_PASSWORD_CHARS} characters"
                ));
            }
            let password = normalize(password);
            let ncryptsec = crate::keyvault::encrypt(
                &identity.secret_key().secret_bytes(),
                &password,
                crate::keyvault::LOG_N,
            )?;
            let entropy = match &identity.source {
                IdentitySource::Phrase(phrase) => {
                    let mnemonic =
                        bip39::Mnemonic::parse_in_normalized(bip39::Language::English, phrase)
                            .map_err(|e| format!("recovery phrase: {e}"))?;
                    let entropy = Zeroizing::new(mnemonic.to_entropy());
                    Some(crate::keyvault::seal(
                        &entropy,
                        &password,
                        crate::keyvault::LOG_N,
                    )?)
                }
                IdentitySource::Nsec(_) => None,
            };
            Secret::Password { ncryptsec, entropy }
        }
    };
    let file = File {
        format: FORMAT.into(),
        version: VERSION,
        pubkey: identity.pubkey.clone(),
        display_name: identity.display_name.clone(),
        secret,
    };
    serde_json::to_string_pretty(&file)
        .map(Zeroizing::new)
        .map_err(|e| format!("serialize backup: {e}"))
}

fn parse(content: &str) -> Result<File, String> {
    if content.len() > MAX_BYTES {
        return Err("that backup is too large to be a Discordia identity".into());
    }
    let file: File = serde_json::from_str(content)
        .map_err(|_| "that file is not a Discordia identity backup".to_string())?;
    if file.format != FORMAT {
        return Err("that file is not a Discordia identity backup".into());
    }
    if file.version != VERSION {
        return Err("that backup was made by a newer Discordia; update to open it".into());
    }
    if !crate::protocol::is_canonical_pubkey(&file.pubkey) {
        return Err("that backup names an invalid key".into());
    }
    Ok(file)
}

pub fn summary(content: &str) -> Result<Summary, String> {
    let file = parse(content)?;
    Ok(Summary {
        display_name: crate::protocol::truncate_username(&file.display_name),
        protected: matches!(file.secret, Secret::Password { .. }),
        pubkey: file.pubkey,
    })
}

pub fn open(content: &str, password: Option<&str>) -> Result<Identity, String> {
    let file = parse(content)?;
    let name = crate::protocol::truncate_username(&file.display_name);
    let identity = match file.secret {
        Secret::None { nsec, phrase } => {
            let identity = Identity::restore_from_private_key(&nsec, &name)?;
            match phrase.and_then(|p| Identity::restore_from_phrase(&p, &name).ok()) {
                Some(from_phrase) if from_phrase.pubkey == identity.pubkey => from_phrase,
                _ => identity,
            }
        }
        Secret::Password { ncryptsec, entropy } => {
            let password = normalize(password.ok_or("this backup needs its password")?);
            let affordable =
                |blob: &str| crate::keyvault::cost(blob).is_some_and(|log_n| log_n <= MAX_LOG_N);
            if !affordable(&ncryptsec) {
                return Err("that backup is locked in a way Discordia can't open".into());
            }
            let bytes = crate::keyvault::decrypt(&ncryptsec, &password)
                .map_err(|_| "wrong password for this backup".to_string())?;
            let mut identity = Identity::restore_from_private_key(hex::encode(*bytes), &name)?;
            if let Some(sealed) = entropy.filter(|sealed| affordable(sealed))
                && let Some(source) =
                    crate::identity::recover_phrase(&sealed, &password, &identity.pubkey)
            {
                identity.source = source;
            }
            identity
        }
    };
    if identity.pubkey != file.pubkey {
        return Err("that backup's key doesn't match the identity it names".into());
    }
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phrase() -> String {
        bip39::Mnemonic::from_entropy(&[1u8; 16])
            .unwrap()
            .to_string()
    }

    fn from_phrase() -> Identity {
        Identity::restore_from_phrase(phrase(), "ana").unwrap()
    }

    fn from_nsec() -> Identity {
        Identity::restore_from_private_key("07".repeat(32), "bo").unwrap()
    }

    #[test]
    fn without_a_password_the_key_and_its_phrase_come_back() {
        let id = from_phrase();
        let content = seal(&id, None).unwrap();
        let back = open(&content, None).unwrap();
        assert_eq!(back.pubkey, id.pubkey);
        assert_eq!(back.display_name, "ana");
        assert_eq!(back.source, id.source);
        assert!(!summary(&content).unwrap().protected);
    }

    #[test]
    fn a_password_opens_it_and_nothing_else_does() {
        let id = from_phrase();
        let content = seal(&id, Some("correct horse")).unwrap();
        assert!(!content.contains("nsec1"));
        assert!(!content.contains(phrase().split(' ').next().unwrap()));
        assert!(summary(&content).unwrap().protected);
        assert!(open(&content, None).is_err());
        assert!(open(&content, Some("wrong horse")).is_err());
        let back = open(&content, Some("correct horse")).unwrap();
        assert_eq!(back.pubkey, id.pubkey);
        assert_eq!(back.source, id.source);
    }

    #[test]
    fn a_key_without_a_phrase_round_trips_both_ways() {
        let id = from_nsec();
        for password in [None, Some("correct horse")] {
            let back = open(&seal(&id, password).unwrap(), password).unwrap();
            assert_eq!(back.pubkey, id.pubkey);
            assert!(matches!(back.source, IdentitySource::Nsec(_)));
        }
    }

    #[test]
    fn a_password_typed_in_another_normal_form_still_opens_it() {
        let id = from_nsec();
        let content = seal(&id, Some("contrase\u{00f1}a1")).unwrap();
        assert!(open(&content, Some("contrasen\u{0303}a1")).is_ok());
    }

    #[test]
    fn a_short_password_is_refused() {
        assert!(seal(&from_nsec(), Some("short")).is_err());
    }

    #[test]
    fn a_file_whose_key_is_not_the_one_it_names_is_refused() {
        let content = seal(&from_nsec(), None).unwrap();
        let swapped = content.replace(&from_nsec().pubkey, &from_phrase().pubkey);
        assert!(open(&swapped, None).is_err());
    }

    #[test]
    fn a_phrase_for_another_key_is_dropped_not_trusted() {
        let mut file: serde_json::Value =
            serde_json::from_str(&seal(&from_nsec(), None).unwrap()).unwrap();
        file["phrase"] = phrase().into();
        let back = open(&file.to_string(), None).unwrap();
        assert_eq!(back.pubkey, from_nsec().pubkey);
        assert!(matches!(back.source, IdentitySource::Nsec(_)));
    }

    #[test]
    fn an_unaffordable_scrypt_cost_is_refused_before_deriving() {
        let id = from_nsec();
        let mut file: serde_json::Value =
            serde_json::from_str(&seal(&id, Some("correct horse")).unwrap()).unwrap();
        let ncryptsec = file["ncryptsec"].as_str().unwrap().to_owned();
        let (hrp, mut data) = bech32::decode(&ncryptsec).unwrap();
        data[1] = 40;
        file["ncryptsec"] = bech32::encode::<bech32::Bech32>(hrp, &data).unwrap().into();
        let before = crate::keyvault::derivation_count();
        assert!(open(&file.to_string(), Some("correct horse")).is_err());
        assert_eq!(crate::keyvault::derivation_count(), before);
    }

    #[test]
    fn names_are_only_what_we_write() {
        let pubkey = from_nsec().pubkey;
        assert!(is_backup_name(&file_name(&pubkey)));
        assert!(!is_backup_name("identity-ABC.json"));
        assert!(!is_backup_name(&file_name(&pubkey.to_uppercase())));
        assert!(!is_backup_name("notes.json"));
    }

    #[test]
    fn foreign_and_future_files_are_refused() {
        assert!(summary("{}").is_err());
        let mut file: serde_json::Value =
            serde_json::from_str(&seal(&from_nsec(), None).unwrap()).unwrap();
        file["version"] = 2.into();
        assert!(summary(&file.to_string()).is_err());
        assert!(summary(&"x".repeat(MAX_BYTES + 1)).is_err());
    }
}
