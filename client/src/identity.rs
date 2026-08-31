use std::path::{Path, PathBuf};

use bech32::{Bech32, Hrp};
use bip39::{Language, Mnemonic};
use hmac::{Hmac, Mac};
use secp256k1::{Keypair, Message, PublicKey, Scalar, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};

const FILE_VERSION: u32 = 1;
const ACTIVE_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentitySource {
    Phrase(String),
    Nsec(String),
}

const MEDIA_KEY_DOMAIN: &[u8] = b"dioxusfun/media-key/v1";

#[derive(Clone)]
pub struct Identity {
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
    pub fn create(display_name: impl Into<String>) -> Result<Self, String> {
        use rand::RngCore;
        let mut entropy = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut entropy);
        let mnemonic = Mnemonic::from_entropy_in(Language::English, &entropy)
            .map_err(|e: bip39::Error| e.to_string())?;
        Self::from_mnemonic(mnemonic, display_name.into())
    }

    pub fn restore_from_phrase(
        seed_phrase: impl AsRef<str>,
        display_name: impl Into<String>,
    ) -> Result<Self, String> {
        let mnemonic =
            Mnemonic::parse_in_normalized(Language::English, seed_phrase.as_ref().trim())
                .map_err(|e: bip39::Error| format!("invalid recovery phrase: {e}"))?;
        Self::from_mnemonic(mnemonic, display_name.into())
    }

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

    pub fn secret_key(&self) -> secp256k1::SecretKey {
        self.secret
    }

    pub fn transport_seed(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"dioxusfun/quic-transport/v1");
        h.update(self.secret.secret_bytes());
        h.finalize().into()
    }

    pub fn shared_secret_with(&self, their_pubkey_hex: &str) -> Result<[u8; 32], String> {
        self.secret_with_domain(their_pubkey_hex, MEDIA_KEY_DOMAIN)
    }

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

    pub fn sign_hex(&self, message: &[u8]) -> String {
        let secp = Secp256k1::new();
        let keypair = Keypair::from_secret_key(&secp, &self.secret);
        let digest: [u8; 32] = Sha256::digest(message).into();
        let msg = Message::from_digest(digest);
        let sig = secp.sign_schnorr_no_aux_rand(&msg, &keypair);
        hex::encode(sig.serialize())
    }

    pub fn nostr_sign_id(&self, id: &[u8; 32]) -> String {
        let secp = Secp256k1::new();
        let keypair = Keypair::from_secret_key(&secp, &self.secret);
        let msg = Message::from_digest(*id);
        hex::encode(secp.sign_schnorr_no_aux_rand(&msg, &keypair).serialize())
    }

    pub fn npub(&self) -> String {
        npub_of(&self.pubkey)
    }

    #[allow(dead_code)]
    pub fn export_nsec(&self) -> String {
        to_bech32("nsec", &self.secret.secret_bytes())
    }

    /// Writes the key under its own pubkey and points `identity.json` at it,
    /// so signing in again is a choice rather than a re-import.
    pub fn save(&self) -> Result<(), String> {
        let dir = identities_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("create identities dir: {e}"))?;
        let stored = Stored {
            version: FILE_VERSION,
            display_name: self.display_name.clone(),
            pubkey: self.pubkey.clone(),
            seed_phrase: match &self.source {
                IdentitySource::Phrase(p) => Some(p.clone()),
                IdentitySource::Nsec(_) => None,
            },
            nsec: match &self.source {
                IdentitySource::Nsec(k) => Some(k.clone()),
                IdentitySource::Phrase(_) => None,
            },
        };
        let content = serde_json::to_string_pretty(&stored)
            .map_err(|e| format!("serialize identity: {e}"))?;
        write_private(&self.key_path(), &content)?;
        set_active(&self.pubkey)
    }

    pub fn load() -> Result<Option<Self>, String> {
        let path = active_path();
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path).map_err(|e| format!("read identity: {e}"))?;
        // Before the folder existed the key itself lived here. Moving it out
        // on the first read is what keeps the pointer free of secrets.
        if let Ok(stored) = serde_json::from_str::<Stored>(&content) {
            let identity = Self::from_stored(stored)?;
            identity.save()?;
            return Ok(Some(identity));
        }
        let active: Active =
            serde_json::from_str(&content).map_err(|e| format!("parse identity: {e}"))?;
        if active.version != ACTIVE_VERSION {
            return Err(format!(
                "unknown identity file version {}; expected {}",
                active.version, ACTIVE_VERSION
            ));
        }
        match detected().into_iter().find(|f| f.pubkey == active.active) {
            Some(found) => Self::from_file(&found.path).map(Some),
            // The key it named is gone, so the pointer is a dead end, not an
            // error to stop the app with.
            None => {
                let _ = std::fs::remove_file(&path);
                Ok(None)
            }
        }
    }

    /// Loads one of the keys `detected` found and makes it the active one,
    /// copying it into the folder if it was sitting loose beside it.
    pub fn sign_in(pubkey: &str) -> Result<Self, String> {
        let found = detected()
            .into_iter()
            .find(|f| f.pubkey == pubkey)
            .ok_or_else(|| "that identity is no longer on this machine".to_string())?;
        let identity = Self::from_file(&found.path)?;
        identity.save()?;
        Ok(identity)
    }

    fn from_file(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path).map_err(|e| format!("read identity: {e}"))?;
        let stored: Stored =
            serde_json::from_str(&content).map_err(|e| format!("parse identity: {e}"))?;
        Self::from_stored(stored)
    }

    fn from_stored(stored: Stored) -> Result<Self, String> {
        if stored.version != FILE_VERSION {
            return Err(format!(
                "unknown identity file version {}; expected {}",
                stored.version, FILE_VERSION
            ));
        }
        if let Some(phrase) = stored.seed_phrase {
            Self::restore_from_phrase(&phrase, stored.display_name)
        } else if let Some(nsec) = stored.nsec {
            Self::restore_from_private_key(&nsec, stored.display_name)
        } else {
            Err("identity file has neither seed_phrase nor nsec".into())
        }
    }

    fn key_path(&self) -> PathBuf {
        identities_dir().join(format!("{}.json", self.pubkey))
    }

    pub fn file_path_display(&self) -> String {
        self.key_path().display().to_string()
    }

    pub fn set_display_name(&mut self, name: impl Into<String>) -> Result<(), String> {
        self.display_name = name.into();
        self.save()
    }
}

/// Every key this machine can sign in with, newest name order, deduplicated by
/// pubkey — the folder wins over a copy left loose in the config directory.
pub fn detected() -> Vec<FoundIdentity> {
    let mut found = Vec::new();
    collect_from(&identities_dir(), &mut found);
    collect_from(&config_dir(), &mut found);
    found.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
            .then_with(|| a.pubkey.cmp(&b.pubkey))
    });
    found
}

/// Signing out leaves the key; this is the one that takes it away.
pub fn forget(pubkey: &str) -> Result<(), String> {
    for found in detected().into_iter().filter(|f| f.pubkey == pubkey) {
        std::fs::remove_file(&found.path).map_err(|e| format!("remove identity: {e}"))?;
    }
    if active_pubkey().as_deref() == Some(pubkey) {
        sign_out()?;
    }
    Ok(())
}

/// Drops the pointer, not the key — the machine forgets who you were, and the
/// setup screen lists you again.
pub fn sign_out() -> Result<(), String> {
    let path = active_path();
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("remove identity: {e}"))?;
    }
    Ok(())
}

/// One key on disk without its secret: enough to draw a row, and to ask for
/// the rest by pubkey when somebody picks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundIdentity {
    pub pubkey: String,
    pub display_name: String,
    pub path: PathBuf,
}

fn collect_from(dir: &Path, out: &mut Vec<FoundIdentity>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(stored) = serde_json::from_str::<Stored>(&content) else {
            continue;
        };
        // Without a secret it is some other file that happens to carry a name
        // and a key — a profile, half a backup, the pointer's own successor.
        if stored.version != FILE_VERSION
            || (stored.seed_phrase.is_none() && stored.nsec.is_none())
            || stored.pubkey.len() != 64
            || out
                .iter()
                .any(|f: &FoundIdentity| f.pubkey == stored.pubkey)
        {
            continue;
        }
        out.push(FoundIdentity {
            pubkey: stored.pubkey,
            display_name: stored.display_name,
            path,
        });
    }
}

fn active_pubkey() -> Option<String> {
    let content = std::fs::read_to_string(active_path()).ok()?;
    serde_json::from_str::<Active>(&content)
        .ok()
        .map(|a| a.active)
}

fn set_active(pubkey: &str) -> Result<(), String> {
    let active = Active {
        version: ACTIVE_VERSION,
        active: pubkey.to_string(),
    };
    let content =
        serde_json::to_string_pretty(&active).map_err(|e| format!("serialize identity: {e}"))?;
    write_private(&active_path(), &content)
}

fn write_private(path: &Path, content: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "identity path has no parent".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
    std::fs::write(path, content).map_err(|e| format!("write identity: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o600);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    Ok(())
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

/// Which of the folder's keys is signed in. A separate shape from `Stored` so
/// a v1 file is told apart by parsing, not by trusting its version field.
#[derive(Serialize, Deserialize)]
struct Active {
    version: u32,
    active: String,
}

pub fn npub_of(pubkey: &str) -> String {
    let bytes = hex::decode(pubkey).unwrap_or_default();
    to_bech32("npub", &bytes)
}

fn to_bech32(hrp: &str, data: &[u8]) -> String {
    let hrp = Hrp::parse(hrp).expect("valid hrp");
    bech32::encode::<Bech32>(hrp, data).expect("bech32 encode")
}

// Thread-local and not the env var, because `DIOXUSFUN_CONFIG_DIR` is
// process-global and another test in this binary sets and clears it.
#[cfg(test)]
thread_local! {
    static TEST_CONFIG_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

pub fn config_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(dir) = TEST_CONFIG_DIR.with(|d| d.borrow().clone()) {
        return dir;
    }
    if let Some(dir) = std::env::var_os("DIOXUSFUN_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("dioxusfun")
}

fn active_path() -> PathBuf {
    config_dir().join("identity.json")
}

fn identities_dir() -> PathBuf {
    config_dir().join("identities")
}

pub fn truncate_pubkey(pubkey: &str) -> String {
    if pubkey.len() <= 12 {
        return pubkey.to_string();
    }
    format!("{}…{}", &pubkey[..8], &pubkey[pubkey.len() - 4..])
}

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

pub fn discriminator(pubkey: &str) -> &str {
    if pubkey.len() <= 4 {
        return pubkey;
    }
    &pubkey[pubkey.len() - 4..]
}

pub fn color_signature(pubkey: &str, n: usize) -> Vec<String> {
    let mut h: u32 = 0;
    for b in pubkey.bytes() {
        h = h.wrapping_mul(31).wrapping_add(b as u32);
    }
    (0..n)
        .map(|i| {
            h = h.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let hue = h % 360;
            let sat = 62 + (h >> 9) % 24;
            let light = 56 + (h >> 17) % 12;
            let _ = i;
            format!("hsl({hue}, {sat}%, {light}%)")
        })
        .collect()
}

pub fn signature_accent(pubkey: &str) -> String {
    color_signature(pubkey, 1)
        .into_iter()
        .next()
        .unwrap_or_else(|| "hsl(30, 70%, 60%)".into())
}

const HARDENED: u32 = 0x8000_0000;
const NIP06_PATH: [u32; 5] = [44 | HARDENED, 1237 | HARDENED, HARDENED, 0, 0];

fn derive_nip06(seed: &[u8]) -> Result<SecretKey, String> {
    let secp = Secp256k1::new();

    let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(b"Bitcoin seed").expect("hmac key length");
    mac.update(seed);
    let master = mac.finalize().into_bytes();
    let mut key = SecretKey::from_slice(&master[..32]).map_err(|e| format!("master key: {e}"))?;
    let mut chain = [0u8; 32];
    chain.copy_from_slice(&master[32..]);

    for &index in &NIP06_PATH {
        let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(&chain).expect("hmac key length");
        if index & HARDENED != 0 {
            mac.update(&[0u8]);
            mac.update(&key.secret_bytes());
        } else {
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
mod store_tests {
    use super::*;

    /// Every test gets its own config root on its own thread, so the folder
    /// scan sees only what the test put there.
    fn sandbox(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dioxusfun-identity-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("sandbox");
        TEST_CONFIG_DIR.with(|d| *d.borrow_mut() = Some(dir.clone()));
        dir
    }

    /// Sign out has to leave something behind, or the list it feeds is always
    /// empty and the whole screen is decoration.
    #[test]
    fn signing_out_keeps_the_key_and_lists_it_again() {
        let root = sandbox("signout");
        let id = Identity::create("malvina").expect("identity");
        id.save().expect("save");
        assert!(
            root.join("identities")
                .join(format!("{}.json", id.pubkey))
                .exists()
        );

        sign_out().expect("sign out");
        assert!(
            Identity::load().expect("load").is_none(),
            "no longer signed in"
        );

        let found = detected();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].pubkey, id.pubkey);
        assert_eq!(found[0].display_name, "malvina");

        let back = Identity::sign_in(&id.pubkey).expect("sign in");
        assert_eq!(back.pubkey, id.pubkey);
        assert_eq!(
            Identity::load().expect("load").map(|i| i.pubkey),
            Some(id.pubkey),
            "picking one makes it the active one"
        );
    }

    /// Forgetting is the destructive half that sign out gave up.
    #[test]
    fn forgetting_removes_the_key_and_the_pointer() {
        let root = sandbox("forget");
        let id = Identity::create("jotace").expect("identity");
        id.save().expect("save");

        forget(&id.pubkey).expect("forget");
        assert!(detected().is_empty());
        assert!(!root.join("identity.json").exists(), "pointer went with it");
        assert!(Identity::load().expect("load").is_none());
    }

    /// Two keys are the point of a list; one must not shadow the other.
    #[test]
    fn both_keys_survive_each_other() {
        sandbox("two");
        let a = Identity::create("aa").expect("a");
        let b = Identity::create("bb").expect("b");
        a.save().expect("save a");
        b.save().expect("save b");

        let found = detected();
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(
            Identity::load().expect("load").map(|i| i.pubkey),
            Some(b.pubkey),
            "the last one saved is the active one"
        );
    }

    /// An install from before the folder existed keeps its key in the pointer
    /// file; the first read has to move it without losing the session.
    #[test]
    fn a_v1_file_migrates_into_the_folder() {
        let root = sandbox("v1");
        let id = Identity::create("legacy").expect("identity");
        let IdentitySource::Phrase(phrase) = &id.source else {
            panic!("expected a phrase")
        };
        let v1 = serde_json::json!({
            "version": 1,
            "display_name": "legacy",
            "pubkey": id.pubkey,
            "seed_phrase": phrase,
        });
        std::fs::write(root.join("identity.json"), v1.to_string()).expect("write v1");

        let loaded = Identity::load().expect("load").expect("still signed in");
        assert_eq!(loaded.pubkey, id.pubkey);
        assert!(
            root.join("identities")
                .join(format!("{}.json", id.pubkey))
                .exists(),
            "the key moved into the folder"
        );
        let pointer = std::fs::read_to_string(root.join("identity.json")).expect("pointer");
        assert!(
            !pointer.contains(phrase.as_str()),
            "the pointer must not keep the secret: {pointer}"
        );
        assert_eq!(detected().len(), 1, "and it is listed once, not twice");
    }

    /// The config folder is full of other json. None of it is an account.
    #[test]
    fn other_config_files_are_not_identities() {
        let root = sandbox("noise");
        std::fs::write(
            root.join("settings.json"),
            r#"{"version":1,"theme":"dark"}"#,
        )
        .expect("settings");
        // The shape of a key file minus the key — a half-written backup.
        std::fs::write(
            root.join("half.json"),
            r#"{"version":1,"display_name":"x","pubkey":"aa","seed_phrase":null}"#,
        )
        .expect("half");
        assert!(detected().is_empty());
    }

    /// Somebody who drops a backup into the folder means it to be an account.
    #[test]
    fn a_key_left_loose_beside_the_folder_is_detected() {
        let root = sandbox("loose");
        let id = Identity::create("dropped").expect("identity");
        let IdentitySource::Phrase(phrase) = &id.source else {
            panic!("expected a phrase")
        };
        std::fs::write(
            root.join("backup.json"),
            serde_json::json!({
                "version": 1,
                "display_name": "dropped",
                "pubkey": id.pubkey,
                "seed_phrase": phrase,
            })
            .to_string(),
        )
        .expect("write backup");

        let found = detected();
        assert_eq!(found.len(), 1, "{found:?}");

        Identity::sign_in(&id.pubkey).expect("sign in");
        assert!(
            root.join("identities")
                .join(format!("{}.json", id.pubkey))
                .exists(),
            "using it files it properly"
        );
        assert_eq!(detected().len(), 1, "and the copy does not double the row");
    }
}

#[cfg(test)]
mod pubkey_input_tests {
    use super::*;

    #[test]
    fn npub_and_hex_are_the_same_key() {
        let id = Identity::create("t").expect("identity");
        let from_hex = pubkey_from_input(&id.pubkey).expect("hex");
        let from_npub = pubkey_from_input(&id.npub()).expect("npub");
        assert_eq!(from_hex, id.pubkey);
        assert_eq!(from_npub, id.pubkey);
    }

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
