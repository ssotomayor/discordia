//! Every failure here is *digital silence*, not noise — indistinguishable
//! from a peer not speaking, which is why three key bugs were invisible.

pub const KEY_VAR: &str = "DISCORDIA_E2EE_KEY";

pub const OFF_VAR: &str = "DISCORDIA_E2EE";

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

fn usable_key(raw: Option<String>) -> Option<String> {
    raw.filter(|k| !k.trim().is_empty())
}

static ACTIVE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn provider() -> &'static livekit::e2ee::key_provider::KeyProvider {
    static PROVIDER: std::sync::OnceLock<livekit::e2ee::key_provider::KeyProvider> =
        std::sync::OnceLock::new();
    PROVIDER.get_or_init(|| {
        // `with_shared_key`, never `new`: `new` derives per-participant keys,
        // which the webview cannot match.
        livekit::e2ee::key_provider::KeyProvider::with_shared_key(
            livekit::e2ee::key_provider::KeyProviderOptions::default(),
            vec![0u8; crate::mediakey::KEY_LEN],
        )
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoomKind {
    Voice,
    Screen,
}

const RING_SLOTS: u32 = 16;

/// Fixed, because the webview holds one key at one index and cannot be told
/// otherwise. Voice straying onto it would break rekeying.
const SCREEN_SLOT: i32 = 0;

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

fn voice_slot(epoch: u32) -> i32 {
    if !overlap_enabled() {
        return SCREEN_SLOT;
    }
    rotating_slot(epoch)
}

fn rotating_slot(epoch: u32) -> i32 {
    (1 + (epoch % (RING_SLOTS - 1))) as i32
}

static ROOMS: std::sync::Mutex<Vec<(RoomKind, std::sync::Weak<livekit::Room>)>> =
    std::sync::Mutex::new(Vec::new());

static VOICE_SLOT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(SCREEN_SLOT);

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

pub fn place_new_voice_publication(room: &livekit::Room) {
    if !enabled() || !overlap_enabled() {
        return;
    }
    let slot = VOICE_SLOT.load(std::sync::atomic::Ordering::Relaxed);
    if slot != SCREEN_SLOT {
        point_senders_at(room, slot);
    }
}

pub fn register_room(room: &std::sync::Arc<livekit::Room>, kind: RoomKind) {
    if !enabled() {
        room.e2ee_manager().set_enabled(false);
        return;
    }
    let mut rooms = ROOMS.lock().expect("e2ee room list");
    rooms.retain(|(_, r)| r.strong_count() > 0);
    rooms.push((kind, std::sync::Arc::downgrade(room)));
    let have_key = ACTIVE.lock().expect("e2ee key lock").is_some();
    room.e2ee_manager().set_enabled(have_key);
    if kind == RoomKind::Voice {
        place_new_voice_publication(room);
    }
    tracing::info!(
        encrypting = room.e2ee_manager().enabled(),
        have_key,
        ?kind,
        "media room registered"
    );
}

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
    provider().set_shared_key(hex_key.as_bytes().to_vec(), SCREEN_SLOT);
    if slot != SCREEN_SLOT {
        provider().set_shared_key(hex_key.as_bytes().to_vec(), slot);
        VOICE_SLOT.store(slot, std::sync::atomic::Ordering::Relaxed);
    }
    {
        let mut rooms = ROOMS.lock().expect("e2ee room list");
        rooms.retain(|(_, r)| r.strong_count() > 0);
        let mut switched = 0usize;
        let mut moved = 0usize;
        for (kind, room) in rooms
            .iter()
            .filter_map(|(k, r)| r.upgrade().map(|r| (*k, r)))
        {
            room.e2ee_manager().set_enabled(true);
            switched += 1;
            if kind == RoomKind::Voice && slot != SCREEN_SLOT {
                moved += point_senders_at(&room, slot);
            }
        }
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

pub fn current_key() -> Option<String> {
    if let Some(k) = ACTIVE.lock().expect("e2ee key lock").clone() {
        return Some(k);
    }
    shared_key().map(hex_or_literal)
}

fn hex_or_literal(key: &str) -> String {
    key.to_string()
}

pub fn room_options() -> Option<livekit::e2ee::E2eeOptions> {
    if !enabled() {
        return None;
    }
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
    use super::{RING_SLOTS, SCREEN_SLOT, rotating_slot, usable_key};

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

    #[test]
    fn voice_never_takes_the_screen_rooms_slot() {
        for epoch in 0..1_000u32 {
            assert_ne!(rotating_slot(epoch), SCREEN_SLOT);
        }
    }

    #[test]
    fn slots_stay_inside_the_ring() {
        for epoch in 0..1_000u32 {
            let slot = rotating_slot(epoch);
            assert!(
                slot > 0 && slot < RING_SLOTS as i32,
                "slot {slot} out of ring"
            );
        }
    }

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

        assert!(super::provider().get_shared_key(0).is_some());
    }

    #[test]
    fn an_empty_key_is_no_key() {
        assert_eq!(usable_key(None), None);
        assert_eq!(usable_key(Some(String::new())), None);
        assert_eq!(usable_key(Some("   ".into())), None);
        assert_eq!(usable_key(Some("hunter2".into())), Some("hunter2".into()));
        assert_eq!(usable_key(Some(" spaced ".into())), Some(" spaced ".into()));
    }
}
