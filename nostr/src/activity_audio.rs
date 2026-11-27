use crate::nip44;
use rand::RngCore;
use secp256k1::{Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyContext {
    pub instance: [u8; 16],
    pub channel: [u8; 16],
    pub epoch: [u8; 16],
    pub request: [u8; 16],
    pub publication: [u8; 16],
}

pub struct AudioKey([u8; 32]);

impl AudioKey {
    pub fn generate() -> Self {
        let mut key = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        Self(key)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for AudioKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted activity audio key>")
    }
}

impl Drop for AudioKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    purpose: String,
    context: KeyContext,
    publisher: String,
    recipient: String,
    key: [u8; 32],
}

impl Drop for Payload {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

const PURPOSE: &str = "discordia-activity-audio-key-v1";
const DENIED: &str = "invalid activity audio key envelope";

fn pubkey(secret: &SecretKey) -> String {
    hex::encode(secret.x_only_public_key(&Secp256k1::new()).0.serialize())
}

pub fn seal(
    secret: &SecretKey,
    recipient: &str,
    context: &KeyContext,
    key: &AudioKey,
) -> Result<String, &'static str> {
    let conversation =
        Zeroizing::new(nip44::conversation_key(secret, recipient).map_err(|_| DENIED)?);
    let payload = Payload {
        purpose: PURPOSE.into(),
        context: context.clone(),
        publisher: pubkey(secret),
        recipient: recipient.into(),
        key: *key.as_bytes(),
    };
    let plaintext = Zeroizing::new(serde_json::to_string(&payload).map_err(|_| DENIED)?);
    nip44::encrypt(&conversation, &plaintext).map_err(|_| DENIED)
}

pub fn open(
    secret: &SecretKey,
    publisher: &str,
    expected: &KeyContext,
    ciphertext: &str,
) -> Result<AudioKey, &'static str> {
    if ciphertext.len() > 2048 {
        return Err(DENIED);
    }
    let conversation =
        Zeroizing::new(nip44::conversation_key(secret, publisher).map_err(|_| DENIED)?);
    let plaintext = Zeroizing::new(nip44::decrypt(&conversation, ciphertext).map_err(|_| DENIED)?);
    let payload: Payload = serde_json::from_str(&plaintext).map_err(|_| DENIED)?;
    if payload.purpose != PURPOSE
        || payload.context != *expected
        || payload.publisher != publisher
        || payload.recipient != pubkey(secret)
    {
        return Err(DENIED);
    }
    Ok(AudioKey(payload.key))
}
