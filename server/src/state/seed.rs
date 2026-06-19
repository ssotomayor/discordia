use std::sync::RwLock;

use uuid::Uuid;

use crate::protocol::{Channel, ChannelKind, Guild};

use super::AppState;

/// Seed an empty default guild with a single text and voice channel. No NPC
/// users, no messages — real users populate the server when they connect.
pub fn populate(state: &AppState) {
    let lobby = Guild {
        id: Uuid::new_v4(),
        name: "Lobby".into(),
        icon: Some("LB".into()),
        // No owner — the seeded Lobby can't be deleted by anyone.
        owner_pubkey: String::new(),
        accent: None,
    };
    let general = Channel {
        id: Uuid::new_v4(),
        guild_id: lobby.id,
        name: "general".into(),
        kind: ChannelKind::Text,
        topic: None,
    };
    let voice = Channel {
        id: Uuid::new_v4(),
        guild_id: lobby.id,
        name: "General Voice".into(),
        kind: ChannelKind::Voice,
        topic: None,
    };

    state.guilds.insert(lobby.id, lobby.clone());
    state
        .channels_by_guild
        .insert(lobby.id, vec![general.id, voice.id]);
    for ch in [&general, &voice] {
        state.channels.insert(ch.id, ch.clone());
        state.messages.insert(ch.id, RwLock::new(Vec::new()));
    }
}
