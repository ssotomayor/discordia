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

/// Set to `0`/`off`/`false` to turn media encryption off entirely.
///
/// An escape hatch, and it earned its place the first time this was tested
/// between two machines: encryption failed in one direction, silently, and
/// there was no way to take it out of the picture to find out whether it was
/// the cause. A feature whose failure mode is silence needs a switch that
/// removes it from the picture, or every unrelated audio problem becomes a
/// suspect too.
pub const OFF_VAR: &str = "DISCORDIA_E2EE";

/// Whether media encryption is allowed to run at all.
///
/// Public because the webview has to know it *before* a key exists: LiveKit's
/// JS SDK can only be given a key provider in the `Room` constructor, so the
/// decision "will this room ever encrypt" is taken at connect time, long before
/// the channel key usually arrives. `off` is the only answer that lets it skip
/// building one.
pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = !matches!(
            std::env::var(OFF_VAR).ok().as_deref(),
            Some("0") | Some("off") | Some("false") | Some("no")
        );
        if !on {
            eprintln!("[e2ee] {OFF_VAR} is off — media is readable by the SFU carrying it");
        }
        on
    })
}

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
        // `with_shared_key`, never `new`. The difference is one boolean deep in
        // libwebrtc's options — `shared_key` — and it is fixed at construction.
        // `new` builds a provider in *per-participant* mode, where every frame
        // cryptor looks its key up by the remote participant's identity; a
        // shared key set on such a provider is simply never consulted. Both
        // ends then report themselves as encrypting, publish and receive frames
        // quite happily, and neither can decode a syllable of the other.
        //
        // That cost three test sessions across two cities to find, because
        // every symptom pointed at key *distribution*, which was by then
        // working perfectly.
        //
        // The key here is a placeholder. It is never used: `register_room`
        // leaves a room disabled until a real key exists, and `apply_key`
        // replaces this one before enabling anything. The constructor simply
        // has no way to say "shared, key to follow".
        livekit::e2ee::key_provider::KeyProvider::with_shared_key(
            livekit::e2ee::key_provider::KeyProviderOptions::default(),
            vec![0u8; crate::mediakey::KEY_LEN],
        )
    })
}

/// Which LiveKit room a registration is for, because the two cannot rekey the
/// same way.
///
/// `Voice` is native end to end — every participant in `voice-{channel}` is
/// somebody's `features::voice` room — so it can move between key-ring slots.
/// `Screen` cannot: the webview is a participant there, the JS
/// `ExternalE2EEKeyProvider.setKey` takes a key and no index, so that room is
/// pinned to slot 0 in both directions. See `voice_slot`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoomKind {
    Voice,
    Screen,
}

/// Slots in the frame cryptor's key ring. `KeyProviderOptions::key_ring_size`
/// defaults to 16 and we do not change it.
const RING_SLOTS: u32 = 16;

/// The slot the screen room — and therefore the webview — always uses.
const SCREEN_SLOT: i32 = 0;

/// Opt in to overlapping voice keys across a rekey.
///
/// **Default off, and it should stay off until it has been watched working
/// between two machines.** The mechanism rests on one behaviour that cannot be
/// read anywhere in this workspace: that a *receiving* frame cryptor picks its
/// key from the index carried in each frame rather than from its own configured
/// index. `FrameCryptor::set_key_index` bottoms out in
/// `e2ee_transformer_->SetKeyIndex` (`webrtc-sys/src/frame_cryptor.cpp:246`) and
/// the transformer lives inside prebuilt libwebrtc. If that assumption is wrong
/// this does not degrade, it silences the call — the failure mode this whole
/// module keeps being bitten by. So it ships as a switch, like `OFF_VAR`.
pub const OVERLAP_VAR: &str = "DISCORDIA_E2EE_OVERLAP";

fn overlap_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = matches!(
            std::env::var(OVERLAP_VAR).ok().as_deref(),
            Some("1") | Some("on") | Some("true") | Some("yes")
        );
        if on {
            eprintln!(
                "[e2ee] {OVERLAP_VAR} is on — voice rekeys will overlap two key-ring slots. \
                 Unverified between machines; if voice goes silent after a rekey, this is \
                 the first thing to turn off."
            );
        }
        on
    })
}

/// Which ring slot voice publishes under at a given epoch.
///
/// Rotates over slots 1..15 and never touches 0, which belongs to the screen
/// room. The point is only that consecutive epochs land in *different* slots:
/// the previous epoch's key stays loaded while the new one is adopted, so a
/// frame still in flight when the rekey lands has a key to be decrypted with.
/// Every member derives this from the same epoch, so nobody has to be told.
///
/// With overlap off this is `SCREEN_SLOT`, i.e. exactly the old behaviour —
/// one slot, overwritten in place, and the gap that comes with it.
fn voice_slot(epoch: u32) -> i32 {
    if !overlap_enabled() {
        return SCREEN_SLOT;
    }
    rotating_slot(epoch)
}

/// The rotation itself, split out because `voice_slot` reads the environment
/// through a `OnceLock` that no test can own — the same reason `usable_key` is
/// its own function.
fn rotating_slot(epoch: u32) -> i32 {
    (1 + (epoch % (RING_SLOTS - 1))) as i32
}

/// Rooms that want to be told when the key changes, and which kind each is.
///
/// A native room decides at *connect* time whether it has an encryption
/// manager at all, and the key almost always arrives later — the first member
/// in a channel generates it only once it is already there. Keeping the rooms
/// here is what lets a key that arrives late still reach them, which is exactly
/// what the JS side does with `setE2eeKey`.
///
/// Weak, so a room that has gone away is dropped rather than kept alive by this
/// list.
static ROOMS: std::sync::Mutex<Vec<(RoomKind, std::sync::Weak<livekit::Room>)>> =
    std::sync::Mutex::new(Vec::new());

/// The slot voice is currently publishing under, so a track published *after* a
/// rekey starts in the right place.
///
/// A frame cryptor is created when a track is published and defaults to slot 0;
/// nothing re-runs `apply_key` for it. Without this a mid-call publish — a mic
/// republished after a device change — would go out under the screen room's
/// key while everyone else was listening on the voice slot.
static VOICE_SLOT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(SCREEN_SLOT);

/// Point a room's *sending* cryptors at a ring slot.
///
/// Only ours: `frame_cryptors()` returns receivers as well, keyed by the remote
/// participant's identity, and a receiver's index is not ours to set — it reads
/// the slot out of the frame it is decrypting. Filtering by our own identity is
/// what separates the two, because `setup_rtp_sender` registers under the local
/// identity and `setup_rtp_receiver` under the remote one.
fn point_senders_at(room: &livekit::Room, slot: i32) -> usize {
    let me = room.local_participant().identity();
    let mut moved = 0usize;
    for ((identity, _sid), cryptor) in room.e2ee_manager().frame_cryptors() {
        if identity == me {
            cryptor.set_key_index(slot);
            moved += 1;
        }
    }
    moved
}

/// Put a freshly published local track on the current voice slot.
///
/// Called after publishing into the voice room. No-op with overlap off, since
/// the slot is then 0 and that is where a new cryptor already starts.
pub fn place_new_voice_publication(room: &livekit::Room) {
    if !enabled() || !overlap_enabled() {
        return;
    }
    let slot = VOICE_SLOT.load(std::sync::atomic::Ordering::Relaxed);
    if slot != SCREEN_SLOT {
        point_senders_at(room, slot);
    }
}

/// Register a freshly connected room so later keys reach it.
pub fn register_room(room: &std::sync::Arc<livekit::Room>, kind: RoomKind) {
    if !enabled() {
        room.e2ee_manager().set_enabled(false);
        return;
    }
    let mut rooms = ROOMS.lock().expect("e2ee room list");
    rooms.retain(|(_, r)| r.strong_count() > 0);
    rooms.push((kind, std::sync::Arc::downgrade(room)));
    // Set the state explicitly, both ways, because the default is the wrong
    // one: `E2eeManager::new` enables itself whenever options are present, and
    // options are now always present. Left alone, a room that connects before
    // any key exists would encrypt its first frames against an empty provider —
    // undecryptable to everyone, including a peer doing exactly the same thing.
    let have_key = ACTIVE.lock().expect("e2ee key lock").is_some();
    room.e2ee_manager().set_enabled(have_key);
    // A room joining mid-epoch has to start on the slot everyone else is
    // already using, not on the 0 its cryptors default to.
    if kind == RoomKind::Voice {
        place_new_voice_publication(room);
    }
    // At `info`, because whether *this* room is encrypting is the one fact that
    // decides whether anyone can hear this peer, and it has been guessed at
    // across three test sessions. `enabled()` is read back from the manager
    // rather than echoed from the line above, so it reports what the SDK
    // believes rather than what we asked for.
    tracing::info!(
        encrypting = room.e2ee_manager().enabled(),
        have_key,
        ?kind,
        "media room registered"
    );
}

/// Adopt a channel key: hand it to every native room, and to the webview.
///
/// Called when a sealed key is opened. Idempotent, because the same key can
/// arrive twice — two members can both decide they are the one to send it.
///
/// The `epoch` is what makes a rekey survivable rather than merely correct. It
/// picks the ring slot voice publishes under (`voice_slot`), so the *previous*
/// epoch's key is still loaded in its own slot while this one is adopted, and a
/// frame in flight across the changeover still has a key waiting for it. The
/// screen room cannot join in — the webview holds one key at one index — so it
/// keeps swapping slot 0 in place, and keeps its gap.
pub fn apply_key(key: &[u8; crate::mediakey::KEY_LEN], epoch: u32) {
    if !enabled() {
        return;
    }
    let hex_key = hex::encode(key);
    {
        let mut active = ACTIVE.lock().expect("e2ee key lock");
        if active.as_deref() == Some(hex_key.as_str()) {
            return;
        }
        *active = Some(hex_key.clone());
    }
    let slot = voice_slot(epoch);
    tracing::info!(epoch, slot, "adopting a new media key");
    // Slot 0 always: the screen room and the webview live there, and the
    // webview cannot be told an index. With overlap off `slot` is also 0 and
    // this single write is the whole story, exactly as before.
    provider().set_shared_key(hex_key.as_bytes().to_vec(), SCREEN_SLOT);
    if slot != SCREEN_SLOT {
        // The overlap itself. Writing a *different* slot rather than
        // overwriting means the previous epoch's key survives this call — that
        // is the entire mechanism, and the reason the old comment here (that a
        // ring is not worth its bookkeeping because an epoch already says which
        // key is current) was answering the wrong question. Which key is
        // current and which keys a receiver may still accept are different
        // questions, and only the second one closes the gap.
        provider().set_shared_key(hex_key.as_bytes().to_vec(), slot);
        VOICE_SLOT.store(slot, std::sync::atomic::Ordering::Relaxed);
    }
    // And switch every live room on. Without this the key reaches the provider
    // and stops there: a room that connected before any key existed has an
    // encryption manager that is disabled, so it publishes in the clear while a
    // peer who connected later publishes encrypted. That asymmetry is silent
    // and one-directional — you hear them, they do not hear you — which is
    // precisely how it was found.
    {
        let mut rooms = ROOMS.lock().expect("e2ee room list");
        rooms.retain(|(_, r)| r.strong_count() > 0);
        let mut switched = 0usize;
        let mut moved = 0usize;
        for (kind, room) in rooms.iter().filter_map(|(k, r)| r.upgrade().map(|r| (*k, r))) {
            room.e2ee_manager().set_enabled(true);
            switched += 1;
            // Senders move last, and only in the voice room. Everything a
            // listener needs is already in the ring by this point, so no frame
            // can arrive under a slot nobody has filled.
            if kind == RoomKind::Voice && slot != SCREEN_SLOT {
                moved += point_senders_at(&room, slot);
            }
        }
        // Zero here means the key arrived before any room existed, which is
        // normal for whoever generates it — the rooms enable themselves when
        // they register. Zero *after* a call is up is the bug this reports.
        tracing::info!(
            rooms = switched,
            senders_moved = moved,
            slot,
            "media key applied to live rooms"
        );
    }
    let js = format!(
        "{}\nwindow.dxScreen.setE2eeKey({});",
        crate::features::screenshare::SCREEN_JS,
        serde_json::to_string(&hex_key).unwrap_or_else(|_| "null".into())
    );
    let _ = dioxus::document::eval(&js);
}

/// The key rooms should connect with, if any.
///
/// Public for the webview's sake. It used to be handed `shared_key()`, which
/// reads the developer env var and nothing else — so on the real path, where
/// the key is distributed by `crate::mediakey`, the webview connected with
/// `null` and never encrypted anything. The native rooms did, which is a
/// mismatch with no error attached to it: a macOS screen share (published
/// natively, encrypted) arrived at a peer's webview as noise, and every camera
/// everywhere went out in the clear.
pub fn current_key() -> Option<String> {
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
    if !enabled() {
        return None;
    }
    // Always attach the options, key or no key. A room built without them has
    // no encryption manager and can never gain one, so a key arriving a second
    // later would reach the provider and stop there. `register_room` decides
    // whether it is switched *on*, and `apply_key` switches it on later.
    if let Some(key) = current_key() {
        provider().set_shared_key(key.as_bytes().to_vec(), 0);
    }
    Some(livekit::e2ee::E2eeOptions {
        encryption_type: livekit::e2ee::EncryptionType::Gcm,
        key_provider: provider().clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::{rotating_slot, usable_key, RING_SLOTS, SCREEN_SLOT};

    /// The one property the overlap depends on: **consecutive epochs never
    /// share a slot**. If they did, adopting a key would overwrite the one that
    /// frames still in flight were encrypted under, and the rekey would gap
    /// exactly as it did before — silently, since everything else would look
    /// right.
    #[test]
    fn consecutive_epochs_land_in_different_slots() {
        for epoch in 0..1_000u32 {
            assert_ne!(
                rotating_slot(epoch),
                rotating_slot(epoch + 1),
                "epoch {epoch} and {} collided",
                epoch + 1
            );
        }
    }

    /// Slot 0 belongs to the screen room, because the webview holds one key at
    /// one index and cannot be told otherwise. Voice straying onto it would
    /// overwrite the key the webview is using mid-call.
    #[test]
    fn voice_never_takes_the_screen_rooms_slot() {
        for epoch in 0..1_000u32 {
            assert_ne!(rotating_slot(epoch), SCREEN_SLOT);
        }
    }

    /// And it stays inside the ring the provider was built with — a slot past
    /// `key_ring_size` is not an error anyone reports, it is a key that is
    /// simply never found.
    #[test]
    fn slots_stay_inside_the_ring() {
        for epoch in 0..1_000u32 {
            let slot = rotating_slot(epoch);
            assert!(slot > 0 && slot < RING_SLOTS as i32, "slot {slot} out of ring");
        }
    }

    /// The provider must be built in *shared key* mode, and the two modes are
    /// told apart only by a flag fixed at construction.
    ///
    /// A provider from `KeyProvider::new` looks up a key per participant
    /// identity, so a shared key set on it is never consulted: both peers
    /// report themselves as encrypting, frames flow, and neither can decode the
    /// other. This asserts the observable difference so the constructor cannot
    /// be swapped back by anyone tidying up.
    #[test]
    fn the_provider_is_in_shared_key_mode() {
        use livekit::e2ee::key_provider::{KeyProvider, KeyProviderOptions};

        let key = vec![7u8; 32];
        let shared = KeyProvider::with_shared_key(KeyProviderOptions::default(), key.clone());
        assert_eq!(
            shared.get_shared_key(0),
            Some(key.clone()),
            "a shared provider holds the key it was given"
        );

        let per_participant = KeyProvider::new(KeyProviderOptions::default());
        per_participant.set_shared_key(key.clone(), 0);
        assert_ne!(
            per_participant.get_shared_key(0),
            Some(key),
            "if this ever matches, the two modes are indistinguishable here and \
             this test protects nothing — check the SDK before trusting it"
        );

        // And ours is the first kind.
        assert!(super::provider().get_shared_key(0).is_some());
    }

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
