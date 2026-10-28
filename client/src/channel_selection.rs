use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::protocol::{ChannelKind, Id};
use crate::state::AppState;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RememberedTextChannel {
    pub server: String,
    pub guild_id: Id,
    pub channel_id: Id,
}

fn current(state: &AppState) -> Option<RememberedTextChannel> {
    if state.dm_mode {
        return None;
    }
    let channel = state.channels.iter().find(|channel| {
        Some(channel.id) == state.selected_channel
            && Some(channel.guild_id) == state.selected_guild
            && channel.kind == ChannelKind::Text
    })?;
    Some(RememberedTextChannel {
        server: state.server_origin.clone()?,
        guild_id: channel.guild_id,
        channel_id: channel.id,
    })
}

fn remember(selections: &mut Vec<RememberedTextChannel>, selection: RememberedTextChannel) {
    if let Some(existing) = selections
        .iter_mut()
        .find(|saved| saved.server == selection.server && saved.guild_id == selection.guild_id)
    {
        *existing = selection;
    } else {
        selections.push(selection);
    }
}

pub(crate) fn remember_current(state: &mut AppState) {
    if let Some(selection) = current(state) {
        remember(&mut state.last_text_channels, selection);
    }
}

pub(crate) fn recalled(state: &AppState, guild_id: Id) -> Option<Id> {
    let server = state.server_origin.as_deref()?;
    let saved = state
        .last_text_channels
        .iter()
        .find(|saved| saved.server == server && saved.guild_id == guild_id)?;
    state
        .channels
        .iter()
        .find(|channel| {
            channel.id == saved.channel_id
                && channel.guild_id == guild_id
                && channel.kind == ChannelKind::Text
        })
        .map(|channel| channel.id)
}

pub(crate) fn use_persistence(mut state: Signal<AppState>) {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let selections = use_memo(move || {
        let state = state.read();
        let mut selections = state.last_text_channels.clone();
        if let Some(selection) = current(&state) {
            remember(&mut selections, selection);
        }
        selections
    });
    use_effect(move || {
        let selections = selections();
        if state.peek().last_text_channels != selections {
            state.write().last_text_channels = selections.clone();
        }
        // Peeking avoids subscribing this writer to the settings it replaces.
        if settings.peek().last_text_channels != selections {
            let mut next = settings.peek().clone();
            next.last_text_channels = selections;
            settings.set(next.clone());
            crate::settings::save(&next);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Channel;

    fn channel(guild_id: Id, kind: ChannelKind, position: u32) -> Channel {
        Channel {
            id: Id::new_v4(),
            guild_id,
            name: format!("channel-{position}"),
            kind,
            topic: None,
            read_only: false,
            slowmode_secs: 0,
            position,
            access: None,
            divider: Default::default(),
            markdown: false,
        }
    }

    fn state() -> (AppState, Id, Id) {
        let mut state = AppState::empty();
        state.server_origin = Some("server-a".into());
        let guild = Id::new_v4();
        let first = channel(guild, ChannelKind::Text, 1);
        let second = channel(guild, ChannelKind::Text, 2);
        let second_id = second.id;
        state.channels = vec![second, first];
        state.selected_guild = Some(guild);
        (state, guild, second_id)
    }

    #[test]
    fn first_visit_uses_first_text_channel_then_restores_the_selected_one() {
        let (mut state, guild, last) = state();
        assert_eq!(state.default_channel_of(guild), Some(state.channels[1].id));
        state.selected_channel = Some(last);
        remember_current(&mut state);
        state.selected_guild = Some(Id::new_v4());
        state.selected_channel = None;
        assert_eq!(state.default_channel_of(guild), Some(last));
    }

    #[test]
    fn dm_and_voice_selection_do_not_replace_a_community_text_channel() {
        let (mut state, guild, last) = state();
        state.selected_channel = Some(last);
        remember_current(&mut state);
        state.dm_mode = true;
        state.selected_channel = Some(Id::new_v4());
        remember_current(&mut state);
        assert_eq!(state.default_channel_of(guild), Some(last));
        let voice = channel(guild, ChannelKind::Voice, 0);
        state.dm_mode = false;
        state.selected_channel = Some(voice.id);
        state.channels.push(voice);
        remember_current(&mut state);
        assert_eq!(state.default_channel_of(guild), Some(last));
        assert_eq!(state.last_text_channels.len(), 1);
    }

    #[test]
    fn saved_channels_are_scoped_to_the_community_and_server() {
        let (mut state, guild, last) = state();
        state.selected_channel = Some(last);
        remember_current(&mut state);
        let other = Id::new_v4();
        let other_channel = channel(other, ChannelKind::Text, 1);
        let other_id = other_channel.id;
        state.channels.push(other_channel);
        state.selected_guild = Some(other);
        state.selected_channel = Some(other_id);
        remember_current(&mut state);
        assert_eq!(state.default_channel_of(guild), Some(last));
        assert_eq!(state.default_channel_of(other), Some(other_id));
        state.server_origin = Some("server-b".into());
        assert_eq!(state.default_channel_of(guild), Some(state.channels[1].id));
        state.server_origin = Some("server-a".into());
        assert_eq!(state.default_channel_of(guild), Some(last));
    }

    #[test]
    fn inaccessible_deleted_or_non_text_saved_channels_fall_back_safely() {
        let (mut state, guild, last) = state();
        state.selected_channel = Some(last);
        remember_current(&mut state);
        state.channels[0].kind = ChannelKind::Voice;
        let first = state.channels[1].id;
        assert_eq!(state.default_channel_of(guild), Some(first));
        state.channels.remove(0);
        assert_eq!(state.default_channel_of(guild), Some(first));
        state.channels.clear();
        assert_eq!(state.default_channel_of(guild), None);
    }

    #[test]
    fn local_settings_round_trip_and_server_cleanup_preserve_the_selection() {
        let (mut state, guild, last) = state();
        state.selected_channel = Some(last);
        state.clear_server_session();
        let saved = crate::settings::ClientSettings {
            last_text_channels: state.last_text_channels.clone(),
            ..Default::default()
        };
        let encoded = serde_json::to_string(&saved).unwrap();
        let restored: crate::settings::ClientSettings = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored.last_text_channels[0].channel_id, last);
        let (mut reopened, _, _) = self::state();
        reopened.last_text_channels = restored.last_text_channels;
        let mut channel = channel(guild, ChannelKind::Text, 4);
        channel.id = last;
        reopened.channels.push(channel);
        assert_eq!(reopened.default_channel_of(guild), Some(last));
        let mut legacy = serde_json::to_value(saved).unwrap();
        legacy.as_object_mut().unwrap().remove("last_text_channels");
        let old: crate::settings::ClientSettings = serde_json::from_value(legacy).unwrap();
        assert!(old.last_text_channels.is_empty());
    }
}
