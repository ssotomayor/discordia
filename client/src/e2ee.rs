//! End-to-end encryption for media (roadmap Stage 3), first slice.
//!
//! Voice, screen video and camera all terminate at an SFU, which decrypts and
//! re-encrypts every frame — in *every* configuration, including a perfectly
//! direct gateway connection. That makes media the one thing a third party can
//! still read once the transport work is done, and it is why this exists.
//!
//! **This slice is the mechanism, not the feature.** The key comes from an
//! environment variable, so both ends have to be told the same secret out of
//! band, and nothing in the UI claims anything. What it settles is the part
//! that cannot be settled by reading: whether the four connections in a channel
//! — three native rooms and one webview — all derive the same key and can
//! decrypt each other. Key *distribution*, which is what turns this into a
//! feature, is the next slice and is the larger half.
//!
//! Why an env var rather than a setting: a setting implies a promise. Until a
//! key can be distributed without a server holding it, there is no promise to
//! make, and a checkbox saying "encrypt my calls" next to a key typed into both
//! machines by hand would be a worse lie than saying nothing.
//!
//! The two SDKs have to agree on derivation or the result is silence rather
//! than an error — frames arrive and decode to noise. They agree on everything
//! visible from here: both default the ratchet salt to `LKFrameEncryptionKey`,
//! and both use PBKDF2/SHA-256. The JS side does 100 000 iterations; the Rust
//! side hands derivation to libwebrtc, where the count is not readable from
//! this repo. That unverifiable step is exactly what the live test is for.

/// The environment variable carrying the shared passphrase.
pub const KEY_VAR: &str = "DISCORDIA_E2EE_KEY";

/// The passphrase every room in this process should use, if any.
///
/// Read once: a key that changed under a running session would leave some
/// tracks undecryptable and others fine, which is a worse state than either.
pub fn shared_key() -> Option<&'static str> {
    static KEY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        let key = usable_key(std::env::var(KEY_VAR).ok());
        match &key {
            Some(_) => eprintln!(
                "[e2ee] {KEY_VAR} is set — media will be encrypted end to end. \
                 Every participant must have the same value or their audio and video \
                 will arrive as noise."
            ),
            None => tracing::debug!("no {KEY_VAR}; media is readable by the SFU as usual"),
        }
        key
    })
    .as_deref()
}

/// What counts as a key.
///
/// An empty variable is not one. `DISCORDIA_E2EE_KEY=` is the obvious way to
/// try switching this off, and treating it as a passphrase would encrypt the
/// room with the empty string — which fails as silence rather than as an error,
/// the failure mode this whole module has to be careful about.
fn usable_key(raw: Option<String>) -> Option<String> {
    raw.filter(|k| !k.trim().is_empty())
}

/// The passphrase every room actually uses, whatever its source.
///
/// Two sources now: `DISCORDIA_E2EE_KEY`, which is the developer path, and a
/// channel key distributed by `crate::mediakey`, which is the real one. A
/// distributed key wins — it is the one the other members are using.
///
/// **Always a hex string, never raw bytes.** The two SDKs derive their frame
/// keys from whatever they are handed, and they never compare notes: the JS
/// side takes a string, the Rust side takes bytes, and handing one the raw 32
/// bytes while the other gets 64 hex characters produces two different keys and
/// a call where nobody can hear anyone. Hex on both sides removes the question.
static ACTIVE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// One key provider for the whole process, so a rekey reaches every native room
/// at once.
///
/// A member holds up to three native connections in a channel — voice, the
/// subscribe-only `#audio` identity, the `#video` publisher — and they must
/// move to a new key together. Three providers would mean three chances to miss
/// one, and missing one is silence rather than an error.
fn provider() -> &'static livekit::e2ee::key_provider::KeyProvider {
    static PROVIDER: std::sync::OnceLock<livekit::e2ee::key_provider::KeyProvider> =
        std::sync::OnceLock::new();
    PROVIDER.get_or_init(|| {
        livekit::e2ee::key_provider::KeyProvider::new(
            livekit::e2ee::key_provider::KeyProviderOptions::default(),
        )
    })
}

/// Adopt a channel key: hand it to every native room, and to the webview.
///
/// Called when a sealed key is opened. Idempotent, because the same key can
/// arrive twice — two members can both decide they are the one to send it.
pub fn apply_key(key: &[u8; crate::mediakey::KEY_LEN]) {
    let hex_key = hex::encode(key);
    {
        let mut active = ACTIVE.lock().expect("e2ee key lock");
        if active.as_deref() == Some(hex_key.as_str()) {
            return;
        }
        *active = Some(hex_key.clone());
    }
    tracing::info!("adopting a new media key");
    // Index 0 throughout: a ring is only worth its bookkeeping if members can
    // disagree about which slot is current, and an epoch already answers that
    // at a level where everyone can see it.
    provider().set_shared_key(hex_key.as_bytes().to_vec(), 0);
    let js = format!(
        "{}\nwindow.dxScreen.setE2eeKey({});",
        crate::features::screenshare::SCREEN_JS,
        serde_json::to_string(&hex_key).unwrap_or_else(|_| "null".into())
    );
    let _ = dioxus::document::eval(&js);
}

/// The key rooms should connect with, if any.
fn current_key() -> Option<String> {
    if let Some(k) = ACTIVE.lock().expect("e2ee key lock").clone() {
        return Some(k);
    }
    shared_key().map(hex_or_literal)
}

/// The developer key is used as typed; a distributed key is already hex. Both
/// end up as a string both SDKs see identically.
fn hex_or_literal(key: &str) -> String {
    key.to_string()
}

/// `RoomOptions.encryption` for a native room, when a key is configured.
///
/// Applied to all three native connections — voice, the screen room's
/// subscribe-only `#audio` identity, and the `#video` publisher — because a
/// room where one participant is encrypting and another is not is a room where
/// people simply cannot hear each other, with nothing to say why.
pub fn room_options() -> Option<livekit::e2ee::E2eeOptions> {
    let key = current_key()?;
    provider().set_shared_key(key.as_bytes().to_vec(), 0);
    Some(livekit::e2ee::E2eeOptions {
        encryption_type: livekit::e2ee::EncryptionType::Gcm,
        key_provider: provider().clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::usable_key;

    /// An empty or blank variable is not a key — see `usable_key`. Tested on
    /// the real function rather than through the environment, which
    /// `shared_key` reads once per process and no test can own.
    #[test]
    fn an_empty_key_is_no_key() {
        assert_eq!(usable_key(None), None);
        assert_eq!(usable_key(Some(String::new())), None);
        assert_eq!(usable_key(Some("   ".into())), None);
        assert_eq!(usable_key(Some("hunter2".into())), Some("hunter2".into()));
        // Not trimmed when it is real: a passphrase with a trailing space is a
        // different passphrase, and quietly trimming one end would produce a
        // room where two people cannot hear each other.
        assert_eq!(usable_key(Some(" spaced ".into())), Some(" spaced ".into()));
    }
}
