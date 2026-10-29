use std::collections::HashSet;

use dioxus::prelude::*;

use crate::nostr::service::{NostrCmd, NostrTx};
use crate::protocol::{Channel, ChannelKind, ClientMessage, Id, Permission};
use crate::state::{AppState, ConnectionStatus, SessionMode, use_app_state, use_gateway};

pub(crate) const INVITE_LIFETIME_SECS: u64 = 7 * 24 * 60 * 60;

pub(crate) fn message_expiry(message: &crate::protocol::Message) -> Option<i64> {
    let invite = parse_voice_invite(&message.content)?;
    let deadline = message
        .created_at
        .timestamp()
        .saturating_add(INVITE_LIFETIME_SECS as i64);
    Some(
        invite
            .expires_at
            .map_or(deadline, |expires| expires.min(deadline)),
    )
}

pub(crate) fn is_expired_message(message: &crate::protocol::Message, now: i64) -> bool {
    message_expiry(message).is_some_and(|expires| now >= expires)
}

pub(super) fn can_invite_to(state: &AppState, channel: &Channel) -> bool {
    state.status == ConnectionStatus::Ready
        && channel.kind == ChannelKind::Voice
        && state.voice.channel_id == Some(channel.id)
        && state.self_user.as_ref().is_some_and(|user| {
            state
                .members
                .iter()
                .any(|m| m.guild_id == channel.guild_id && m.user.pubkey == user.pubkey && !m.bot)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nostr::nip02::Contact;
    use crate::protocol::{ChannelAccess, Member, Role, User};

    fn fixture() -> (AppState, Channel) {
        let mut state = AppState::empty();
        state.status = ConnectionStatus::Ready;
        state.self_user = Some(User {
            pubkey: "me".into(),
            username: "Me".into(),
        });
        let guild = Id::new_v4();
        state.guilds.push(
            serde_json::from_value(serde_json::json!({
                "id": guild, "name": "Community", "owner_pubkey": "owner"
            }))
            .unwrap(),
        );
        let channel: Channel = serde_json::from_value(serde_json::json!({
            "id": Id::new_v4(), "guild_id": guild, "name": "Lounge", "kind": "voice"
        }))
        .unwrap();
        state.channels.push(channel.clone());
        state.voice.channel_id = Some(channel.id);
        for (pk, name, bot) in [
            ("me", "Me", false),
            ("alice", "Alice", false),
            ("owner", "Owner", false),
            ("bot", "Bot", true),
        ] {
            state.members.push(Member {
                user: User {
                    pubkey: pk.into(),
                    username: name.into(),
                },
                guild_id: guild,
                online: true,
                bot,
                roles: vec![],
                xp: 0,
                look: Default::default(),
            });
        }
        (state, channel)
    }

    #[test]
    fn members_can_invite_existing_members_without_creating_a_guild_invite() {
        let (state, channel) = fixture();
        assert!(can_invite_to(&state, &channel));
        let text = invitation(&state, &channel, "alice", None, None).unwrap();
        assert!(text.contains("Lounge") && text.contains("Community"));
        assert!(
            invitation(
                &state,
                &channel,
                "outside",
                Some("quic://server"),
                Some("code")
            )
            .is_none()
        );
        assert!(!eligible(&state, &channel, "me"));
        assert!(!eligible(&state, &channel, "bot"));
    }

    #[test]
    fn outside_invitations_require_permission_and_a_complete_connection_message() {
        let (mut state, channel) = fixture();
        state.guilds[0].owner_pubkey = "me".into();
        assert!(invitation(&state, &channel, "outside", None, Some("code")).is_none());
        assert!(invitation(&state, &channel, "outside", Some("quic://server"), None).is_none());
        let text = invitation(
            &state,
            &channel,
            "outside",
            Some("quic://server"),
            Some("invite-code"),
        )
        .unwrap();
        for detail in ["Lounge", "Community", "quic://server", "invite-code"] {
            assert!(text.contains(detail), "missing {detail}");
        }
        state.guilds[0].owner_pubkey = "owner".into();
        assert!(
            invitation(
                &state,
                &channel,
                "outside",
                Some("quic://server"),
                Some("invite-code")
            )
            .is_none()
        );
    }

    #[test]
    fn hidden_channels_only_offer_members_with_current_access() {
        let (mut state, mut channel) = fixture();
        channel.access = Some(ChannelAccess::default());
        assert!(!eligible(&state, &channel, "alice"));
        assert!(eligible(&state, &channel, "owner"));
        assert!(!eligible(&state, &channel, "outside"));
        channel.access.as_mut().unwrap().users.push("alice".into());
        assert!(eligible(&state, &channel, "alice"));
        channel.access.as_mut().unwrap().users.clear();
        let role = Id::new_v4();
        state.members[1].roles.push(role);
        channel.access.as_mut().unwrap().roles.push(role);
        assert!(eligible(&state, &channel, "alice"));
        channel.access.as_mut().unwrap().roles.clear();
        state.roles.insert(
            channel.guild_id,
            vec![Role {
                id: role,
                guild_id: channel.guild_id,
                name: "Moderator".into(),
                color: None,
                permissions: vec![Permission::ManageChannels],
                position: 0,
            }],
        );
        assert!(eligible(&state, &channel, "alice"));
        state.roles.clear();
        assert!(!eligible(&state, &channel, "alice"));
    }

    #[test]
    fn recipient_search_deduplicates_sources_and_uses_current_names() {
        let (mut state, channel) = fixture();
        state.contacts.contacts.push(Contact {
            pubkey: "alice".into(),
            petname: None,
            relay: None,
        });
        state.dms.push(crate::state::DmInfo {
            channel_id: Id::new_v4(),
            other_pubkey: "alice".into(),
        });
        assert_eq!(
            recipients(&state, &channel, "ALICE"),
            vec![("alice".into(), "Alice".into())]
        );
        state.note_name("alice", "Renamed".into(), 100);
        assert_eq!(
            recipients(&state, &channel, "renam"),
            vec![("alice".into(), "Renamed".into())]
        );
        state.mutes = state.mutes.with("alice");
        assert!(recipients(&state, &channel, "renam").is_empty());
    }

    #[test]
    fn stale_sessions_and_memberships_cannot_send_invitations() {
        let (mut state, channel) = fixture();
        state.status = ConnectionStatus::Disconnected;
        assert!(invitation(&state, &channel, "alice", None, None).is_none());
        state.status = ConnectionStatus::Ready;
        state.members.retain(|m| m.user.pubkey != "me");
        assert!(invitation(&state, &channel, "alice", None, None).is_none());
    }

    #[test]
    fn send_button_rechecks_access_and_queues_one_dm_without_joining_voice() {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        use dioxus::core::{AttributeValue, Mutation};
        use std::{any::Any, cell::RefCell, rc::Rc};
        use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
        type Export = Rc<
            RefCell<
                Option<(
                    Signal<AppState>,
                    UnboundedReceiver<NostrCmd>,
                    UnboundedReceiver<ClientMessage>,
                )>,
            >,
        >;
        let export: Export = Rc::default();
        let mut dom = VirtualDom::new_with_props(
            |export: Export| {
                let state = use_signal(|| fixture().0);
                let (nostr, gateway) = use_hook(|| {
                    let (nostr, dm) = NostrTx::test_channel_pair();
                    let (tx, gateway) = unbounded_channel();
                    *export.borrow_mut() = Some((state, dm, gateway));
                    (nostr, crate::state::GatewayTx(tx))
                });
                provide_context(state);
                provide_context(nostr);
                provide_context(gateway);
                provide_context(SessionMode::Remote {
                    server_url: "wss://example.com".into(),
                });
                let channel_id = state.peek().channels[0].id;
                rsx! { VoiceInviteDialog { channel_id, on_close: |_| {} } }
            },
            export.clone(),
        );
        let edits = dom.rebuild_to_vec();
        let button = edits
            .edits
            .iter()
            .find_map(|edit| match edit {
                Mutation::SetAttribute {
                    name: "data-voice-invite-peer",
                    value: AttributeValue::Text(value),
                    id,
                    ..
                } if *value == "alice" => Some(*id),
                _ => None,
            })
            .unwrap();
        let (mut state, mut dm, mut gateway) = export.borrow_mut().take().unwrap();
        let click = |dom: &VirtualDom| {
            let data: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
                dioxus::html::SerializedMouseData::default(),
            )));
            dom.runtime()
                .handle_event("click", Event::new(data, true), button);
        };
        state.write().channels[0].access = Some(ChannelAccess::default());
        click(&dom);
        assert!(dm.try_recv().is_err());
        state.write().channels[0].access = None;
        click(&dom);
        match dm.try_recv().unwrap() {
            NostrCmd::Send {
                peer,
                text,
                reply_to,
                emoji,
                replaces,
            } => {
                assert_eq!(peer, "alice");
                assert!(text.contains("Lounge") && text.contains("Community"));
                assert!(reply_to.is_none() && emoji.is_empty() && replaces.is_none());
            }
            _ => panic!("expected an invitation DM"),
        }
        click(&dom);
        assert!(
            dm.try_recv().is_err(),
            "double clicks must not duplicate invitations"
        );
        assert!(
            gateway.try_recv().is_err(),
            "inviting must not join or move voice"
        );
    }

    #[test]
    fn parse_and_strip_voice_invite_roundtrips() {
        let (state, channel) = fixture();
        let text = invitation(&state, &channel, "alice", None, None).unwrap();
        let parsed = parse_voice_invite(&text).expect("must parse voice invite");
        assert_eq!(parsed.guild_id, channel.guild_id);
        assert_eq!(parsed.channel_id, channel.id);
        assert_eq!(parsed.guild_name, "Community");
        assert_eq!(parsed.channel_name, "Lounge");
        assert!(parsed.server.is_none());
        assert!(parsed.code.is_none());

        let stripped = strip_voice_invite(&text);
        assert!(!stripped.contains("discordia://voice/"));
        assert!(stripped.contains("Lounge"));
        assert!(stripped.contains("Community"));
    }

    fn link(channel: &Channel) -> VoiceInviteInfo {
        VoiceInviteInfo {
            guild_id: channel.guild_id,
            channel_id: channel.id,
            guild_name: "Community".into(),
            channel_name: "Lounge".into(),
            server: Some("wss://inviting.example".into()),
            code: Some("invite-from-target".into()),
            expires_at: None,
        }
    }

    #[test]
    fn an_invite_code_routes_to_its_known_server_before_using_the_gateway() {
        let (mut state, channel) = fixture();
        state.guilds.clear();
        let current = SessionMode::Remote {
            server_url: "wss://other.example".into(),
        };
        let target = crate::known_servers::KnownServer::new(SessionMode::Remote {
            server_url: "wss://inviting.example".into(),
        });
        assert_eq!(
            invite_route(
                &state,
                Some(&current),
                std::slice::from_ref(&target),
                &link(&channel)
            ),
            InviteRoute::Switch(target)
        );
        assert!(state.voice_invite_on_ready.is_none());
        let mut invite = link(&channel);
        for address in [
            "wss://new.example",
            "quic://aabbcc@192.0.2.1:9000;[2001:db8::1]:9000",
        ] {
            invite.server = Some(address.into());
            assert_eq!(
                invite_route(&state, Some(&current), &[], &invite),
                InviteRoute::Switch(crate::known_servers::KnownServer::new(
                    SessionMode::Remote {
                        server_url: address.into(),
                    }
                ))
            );
        }
    }

    #[test]
    fn only_the_issuing_ready_server_can_use_a_code_without_switching() {
        let (mut state, channel) = fixture();
        state.guilds.clear();
        let current = SessionMode::Remote {
            server_url: "wss://inviting.example".into(),
        };
        let mut invite = link(&channel);
        assert_eq!(
            invite_route(&state, Some(&current), &[], &invite),
            InviteRoute::Current
        );
        state.status = ConnectionStatus::Reconnecting;
        assert_eq!(
            invite_route(&state, Some(&current), &[], &invite),
            InviteRoute::Unavailable
        );
        state.status = ConnectionStatus::Ready;
        invite.server = None;
        assert_eq!(
            invite_route(&state, Some(&current), &[], &invite),
            InviteRoute::Unavailable
        );
    }

    #[test]
    fn switching_then_ready_joins_the_community_before_the_requested_voice_channel() {
        let (mut state, channel) = fixture();
        let guild = state.guilds[0].clone();
        let pending = crate::state::PendingVoiceInvite {
            guild_id: channel.guild_id,
            channel_id: channel.id,
            code: Some("target-code".into()),
            expires_at: None,
        };
        state.prepare_server_switch(Some(guild.id), Some(pending));
        assert!(state.resume_voice_invite(None).is_none());
        state.status = ConnectionStatus::Ready;
        assert!(
            matches!(state.resume_voice_invite(None), Some(ClientMessage::JoinByInvite { code, .. }) if code == "target-code")
        );
        assert!(state.resume_voice_invite(Some(Id::new_v4())).is_none());
        assert!(state.voice_invite_on_ready.is_some());
        state.guilds.push(guild);
        state.channels.push(channel.clone());
        assert!(
            matches!(state.resume_voice_invite(Some(channel.guild_id)), Some(ClientMessage::JoinVoice { channel_id, .. }) if channel_id == channel.id)
        );
        assert_eq!(state.selected_guild, Some(channel.guild_id));
        assert_eq!(state.selected_channel, Some(channel.id));
        assert!(state.voice_invite_on_ready.is_none());
        assert!(state.resume_voice_invite(None).is_none());
    }

    #[test]
    fn existing_members_skip_the_code_and_disconnect_clears_the_pending_invite() {
        let (mut state, channel) = fixture();
        let pending = crate::state::PendingVoiceInvite {
            guild_id: channel.guild_id,
            channel_id: channel.id,
            code: Some("unused".into()),
            expires_at: None,
        };
        assert!(
            matches!(state.follow_voice_invite(pending.clone()), Some(ClientMessage::JoinVoice { channel_id, .. }) if channel_id == channel.id)
        );
        assert!(state.open_guild_on_ready.is_none());
        state.guilds.clear();
        assert!(matches!(
            state.follow_voice_invite(pending.clone()),
            Some(ClientMessage::JoinByInvite { .. })
        ));
        assert!(state.open_guild_on_ready.is_none());
        state.voice_invite_on_ready = Some(pending);
        state.clear_server_session();
        assert!(state.voice_invite_on_ready.is_none());
    }

    #[test]
    fn invitations_expire_after_seven_days_and_history_cannot_restore_them() {
        let (mut state, channel) = fixture();
        let before = chrono::Utc::now().timestamp();
        let content = invitation(&state, &channel, "alice", None, None).unwrap();
        let parsed = parse_voice_invite(&content).unwrap();
        let deadline = parsed.expires_at.unwrap();
        assert!(deadline >= before + INVITE_LIFETIME_SECS as i64);
        assert!(deadline <= chrono::Utc::now().timestamp() + INVITE_LIFETIME_SECS as i64);
        let mut message: crate::protocol::Message = serde_json::from_value(serde_json::json!({
            "id": Id::new_v4(), "channel_id": channel.id,
            "author": {"pubkey":"alice", "username":"Alice"},
            "content": content, "created_at": chrono::Utc::now(),
        }))
        .unwrap();
        assert!(!is_expired_message(&message, deadline - 1));
        assert!(is_expired_message(&message, deadline));
        message.content = format!("discordia://voice/{}/{}", channel.guild_id, channel.id);
        message.created_at = chrono::Utc::now() - chrono::Duration::days(8);
        assert!(is_expired_message(&message, before));
        assert!(!state.insert_message(channel.id, message.clone()));
        state.merge_history(channel.id, vec![message.clone()]);
        assert!(state.messages.get(&channel.id).is_none_or(Vec::is_empty));
        message.content = "An ordinary message stays in the chat".into();
        assert!(state.insert_message(channel.id, message));
    }

    #[test]
    fn an_expired_pending_invite_cannot_join_after_a_slow_switch() {
        let (mut state, channel) = fixture();
        assert!(
            state
                .follow_voice_invite(crate::state::PendingVoiceInvite {
                    guild_id: channel.guild_id,
                    channel_id: channel.id,
                    code: Some("old".into()),
                    expires_at: Some(chrono::Utc::now().timestamp() - 1),
                })
                .is_none()
        );
        assert!(state.voice_invite_on_ready.is_none());
    }
}

fn eligible(state: &AppState, channel: &Channel, peer: &str) -> bool {
    if !can_invite_to(state, channel)
        || state.self_user.as_ref().is_some_and(|u| u.pubkey == peer)
        || state.mutes.contains(peer)
        || state.members.iter().any(|m| m.user.pubkey == peer && m.bot)
    {
        return false;
    }
    let member = state
        .members
        .iter()
        .find(|m| m.guild_id == channel.guild_id && m.user.pubkey == peer && !m.bot);
    let Some(member) = member else {
        return channel.access.is_none() && super::invite::can_invite(state, channel.guild_id);
    };
    let Some(access) = &channel.access else {
        return true;
    };
    access.users.iter().any(|pk| pk == peer)
        || access.roles.iter().any(|role| member.roles.contains(role))
        || state
            .guilds
            .iter()
            .any(|g| g.id == channel.guild_id && g.owner_pubkey == peer)
        || state.roles_of(channel.guild_id).iter().any(|role| {
            member.roles.contains(&role.id)
                && role.permissions.contains(&Permission::ManageChannels)
        })
}

fn recipients(state: &AppState, channel: &Channel, query: &str) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let query = query.trim().to_lowercase();
    let mut peers: Vec<_> = state
        .contacts
        .contacts
        .iter()
        .map(|c| c.pubkey.as_str())
        .chain(state.dms.iter().map(|dm| dm.other_pubkey.as_str()))
        .chain(
            state
                .members_of(channel.guild_id)
                .into_iter()
                .map(|m| m.user.pubkey.as_str()),
        )
        .filter(|pk| seen.insert((*pk).to_owned()) && eligible(state, channel, pk))
        .map(|pk| (pk.to_owned(), state.display_name(pk)))
        .filter(|(pk, name)| {
            query.is_empty() || name.to_lowercase().contains(&query) || pk.contains(&query)
        })
        .collect();
    peers.sort_by(|a, b| {
        a.1.to_lowercase()
            .cmp(&b.1.to_lowercase())
            .then_with(|| a.0.cmp(&b.0))
    });
    peers
}

fn invitation(
    state: &AppState,
    channel: &Channel,
    peer: &str,
    address: Option<&str>,
    code: Option<&str>,
) -> Option<String> {
    if !eligible(state, channel, peer) {
        return None;
    }
    let guild = state.guilds.iter().find(|g| g.id == channel.guild_id)?;
    let mut url = url::Url::parse(&format!(
        "discordia://voice/{}/{}",
        channel.guild_id, channel.id
    ))
    .ok()?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("g", &guild.name);
        pairs.append_pair("c", &channel.name);
        pairs.append_pair(
            "exp",
            &(chrono::Utc::now().timestamp() + INVITE_LIFETIME_SECS as i64).to_string(),
        );
        if let Some(srv) = address {
            pairs.append_pair("server", srv);
        }
        if let Some(c) = code {
            pairs.append_pair("code", c);
        }
    }
    let uri = url.as_str();
    if state
        .members_of(channel.guild_id)
        .into_iter()
        .any(|m| m.user.pubkey == peer)
    {
        Some(format!(
            "{}\n{uri}",
            crate::i18n::text(
                "You're invited to voice channel {0} in {1} on Discordia.\nOpen {1}, then select voice channel {0}.",
                &[channel.name.clone(), guild.name.clone()],
            )
        ))
    } else {
        Some(format!(
            "{}\n{uri}",
            crate::i18n::text(
                "You're invited to voice channel {0} in {1} on Discordia.\n1. Add the server: {2}\n2. Join the community with invite code: {3}\n3. Select voice channel: {0}",
                &[
                    channel.name.clone(),
                    guild.name.clone(),
                    address?.to_owned(),
                    code?.to_owned(),
                ],
            )
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceInviteInfo {
    pub guild_id: Id,
    pub channel_id: Id,
    pub guild_name: String,
    pub channel_name: String,
    pub server: Option<String>,
    pub code: Option<String>,
    pub expires_at: Option<i64>,
}

pub fn parse_voice_invite(content: &str) -> Option<VoiceInviteInfo> {
    let pos = content.find("discordia://voice/")?;
    let raw = content[pos..].split_whitespace().next()?;
    let url = url::Url::parse(raw).ok()?;
    if url.scheme() != "discordia" || url.host_str() != Some("voice") {
        return None;
    }
    let mut segs = url.path_segments()?;
    let guild_id = Id::parse_str(segs.next()?).ok()?;
    let channel_id = Id::parse_str(segs.next()?).ok()?;
    let mut guild_name = String::new();
    let mut channel_name = String::new();
    let mut server = None;
    let mut code = None;
    let mut expires_at = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "g" => guild_name = v.into_owned(),
            "c" => channel_name = v.into_owned(),
            "server" => server = Some(v.into_owned()),
            "code" => code = Some(v.into_owned()),
            "exp" => expires_at = Some(v.parse().ok()?),
            _ => {}
        }
    }
    Some(VoiceInviteInfo {
        guild_id,
        channel_id,
        guild_name,
        channel_name,
        server,
        code,
        expires_at,
    })
}

pub fn strip_voice_invite(content: &str) -> String {
    if let Some(pos) = content.find("discordia://voice/") {
        content[..pos].trim_end().to_string()
    } else {
        content.to_string()
    }
}

#[derive(Debug, PartialEq)]
enum InviteRoute {
    Current,
    Switch(crate::known_servers::KnownServer),
    Unavailable,
}

fn invite_route(
    state: &AppState,
    current: Option<&SessionMode>,
    known: &[crate::known_servers::KnownServer],
    invite: &VoiceInviteInfo,
) -> InviteRoute {
    if state.status == ConnectionStatus::Ready
        && state.guilds.iter().any(|g| g.id == invite.guild_id)
    {
        return InviteRoute::Current;
    }
    let current_key = current.map(crate::known_servers::server_key);
    let current_matches = current.is_some_and(|mode| {
        matches!(mode, SessionMode::Remote { server_url } if invite.server.as_ref() == Some(server_url))
            || super::invite::server_addresses(
                mode,
                state.host_info.as_ref(),
                state.resolved_share.as_deref(),
            )
            .iter()
            .any(|address| Some(address) == invite.server.as_ref())
    });
    let target = known
        .iter()
        .find(|server| server.guilds.iter().any(|g| g.id == invite.guild_id))
        .or_else(|| {
            known.iter().find(|server| {
                matches!(&server.mode, SessionMode::Remote { server_url } if invite.server.as_ref() == Some(server_url))
            })
        });
    if current_matches || target.is_some_and(|server| Some(server.key()) == current_key) {
        return if state.status == ConnectionStatus::Ready && invite.code.is_some() {
            InviteRoute::Current
        } else {
            InviteRoute::Unavailable
        };
    }
    if let Some(target) = target {
        return InviteRoute::Switch(target.clone());
    }
    if let Some(address) = &invite.server
        && (crate::protocol::parse_quic_share(address).is_some()
            || url::Url::parse(address).is_ok_and(|url| matches!(url.scheme(), "ws" | "wss")))
    {
        return InviteRoute::Switch(crate::known_servers::KnownServer::new(
            SessionMode::Remote {
                server_url: address.clone(),
            },
        ));
    }
    InviteRoute::Unavailable
}

#[component]
pub fn VoiceInviteCardRow(invite: VoiceInviteInfo) -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let rail = try_consume_context::<crate::features::server_rail::ServerRail>();
    let mode = try_consume_context::<SessionMode>();
    let mobile_tab = try_consume_context::<Signal<super::workspace::MobileTab>>();

    let is_connected_here = state.read().connected_voice_channel() == Some(invite.channel_id);
    let channel_title = if invite.channel_name.is_empty() {
        crate::i18n::text("Voice channel", &[])
    } else {
        invite.channel_name.clone()
    };
    let guild_title = if invite.guild_name.is_empty() {
        crate::i18n::text("Community", &[])
    } else {
        invite.guild_name.clone()
    };

    let on_join = move |_| {
        if invite
            .expires_at
            .is_some_and(|expires| chrono::Utc::now().timestamp() >= expires)
        {
            state.write().error_toast =
                Some(crate::i18n::text("This invitation has expired.", &[]));
            return;
        }
        if state.peek().dm_call.is_some() {
            state.write().error_toast = Some(crate::i18n::text(
                "End your DM call before joining a voice channel.",
                &[],
            ));
            return;
        }
        let s = state.peek();
        if s.connected_voice_channel() == Some(invite.channel_id) {
            return;
        }
        let route = match rail {
            Some(rail) => invite_route(&s, mode.as_ref(), &rail.known.peek(), &invite),
            None => invite_route(&s, mode.as_ref(), &[], &invite),
        };
        drop(s);
        let pending = crate::state::PendingVoiceInvite {
            guild_id: invite.guild_id,
            channel_id: invite.channel_id,
            code: invite.code.clone(),
            expires_at: invite.expires_at,
        };
        if route == InviteRoute::Current {
            if let Some(mut tab) = mobile_tab {
                tab.set(super::workspace::MobileTab::Chat);
            }
            let mut s = state.write();
            let join_msg = s.follow_voice_invite(pending);
            drop(s);
            if let Some(message) = join_msg {
                gateway.send(message);
            }
        } else if let (InviteRoute::Switch(server), Some(rail)) = (route, rail) {
            rail.request_voice_invite(server, pending);
        } else {
            state.write().error_toast = Some(crate::i18n::text(
                "You need to join this community to enter the voice channel.",
                &[],
            ));
        }
    };

    rsx! {
        div {
            class: "flex items-center justify-between gap-3 p-3 mt-1.5 rounded-xl border border-[var(--border)] bg-[var(--panel2)] max-w-md",
            div { class: "flex items-center gap-3 min-w-0",
                div {
                    class: "w-9 h-9 rounded-lg bg-[var(--accent-soft)] text-[var(--accent)] flex items-center justify-center shrink-0",
                    span {
                        class: "w-5 h-5",
                        dangerous_inner_html: crate::features::icons::SPEAKER,
                    }
                }
                div { class: "flex-1 min-w-0",
                    div { class: "font-semibold text-sm truncate text-[var(--text)]", "{channel_title}" }
                    div { class: "text-xs text-[var(--text-dim)] truncate", "{guild_title}" }
                }
            }
            if is_connected_here {
                span {
                    class: "text-xs font-medium text-[var(--up)] px-3 py-1.5 rounded-lg bg-green-500/10 shrink-0",
                    {crate::i18n::text("Connected", &[])}
                }
            } else {
                button {
                    r#type: "button",
                    class: "px-3 py-1.5 rounded-lg bg-[var(--accent)] text-black font-semibold text-xs hover:brightness-110 active:scale-[0.98] transition-all shrink-0",
                    onclick: on_join,
                    {crate::i18n::text("Join voice channel", &[])}
                }
            }
        }
    }
}

#[component]
pub(super) fn VoiceInviteDialog(channel_id: Id, on_close: EventHandler<()>) -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let nostr = use_context::<NostrTx>();
    let mode = use_context::<SessionMode>();
    let mut query = use_signal(String::new);
    let mut queued = use_signal(HashSet::<String>::new);
    let mut error = use_signal(|| false);
    use_hook(move || {
        let guild = state
            .peek()
            .channels
            .iter()
            .find(|c| c.id == channel_id)
            .map(|c| c.guild_id);
        if let Some(guild_id) = guild.filter(|gid| super::invite::can_invite(&state.peek(), *gid)) {
            state.write().invites.remove(&guild_id);
            gateway.send(ClientMessage::CreateInvite {
                guild_id,
                rotate: false,
                expires_in_secs: Some(INVITE_LIFETIME_SECS),
                max_uses: None,
            });
        }
    });
    let s = state.read();
    let channel = s.channels.iter().find(|c| c.id == channel_id).cloned();
    let Some(channel) = channel.filter(|c| can_invite_to(&s, c)) else {
        return rsx! { div { class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            div { class: "p-4 bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg",
                p { {crate::i18n::text("This voice channel is no longer available.", &[])} }
                button { onclick: move |_| on_close.call(()), {crate::i18n::text("Close", &[])} }
            }
        } };
    };
    let peers = recipients(&s, &channel, &query());
    let addresses =
        super::invite::server_addresses(&mode, s.host_info.as_ref(), s.resolved_share.as_deref());
    let address = addresses
        .iter()
        .find(|a| a.contains("://"))
        .or_else(|| addresses.first())
        .cloned();
    let code = s.invites.get(&channel.guild_id).cloned();
    drop(s);
    rsx! {
        div { class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: move |_| on_close.call(()),
            section { class: "dxf-modal-in w-[22rem] flex flex-col p-4 gap-3 bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl",
                style: "max-height:80vh; max-width:calc(100vw - 2rem);",
                role: "dialog", aria_modal: "true", aria_label: crate::i18n::text("Invite to voice by DM", &[]),
                onclick: move |e| e.stop_propagation(),
                header { class: "flex items-center gap-2",
                    h3 { class: "flex-1 min-w-0 truncate text-sm font-medium", {crate::i18n::text("Invite to {0}", std::slice::from_ref(&channel.name))} }
                    button { r#type: "button", aria_label: crate::i18n::text("Close", &[]), onclick: move |_| on_close.call(()), "✕" }
                }
                p { class: "text-xs text-[var(--text-muted)]", {crate::i18n::text("Send a private invitation with the voice channel and connection details.", &[])} }
                p { class: "text-xs text-[var(--text-dim)]", {crate::i18n::text("Invitations expire after 7 days and disappear from the chat.", &[])} }
                if channel.access.is_some() {
                    p { class: "text-xs text-[var(--text-dim)]", {crate::i18n::text("Only members with access to this channel are shown.", &[])} }
                }
                input { r#type: "search", value: "{query}",
                    class: "w-full rounded border border-[var(--border)] bg-[var(--panel2)] p-2 text-sm",
                    aria_label: crate::i18n::text("Search people", &[]), placeholder: crate::i18n::text("Search people", &[]),
                    oninput: move |e| query.set(e.value()),
                }
                if error() { p { role: "alert", class: "text-xs text-[var(--danger)]", {crate::i18n::text("Could not queue the invitation. Try again.", &[])} } }
                div { class: "min-h-0 overflow-y-auto flex flex-col gap-1",
                    if peers.is_empty() { p { class: "text-xs text-[var(--text-dim)] p-2", {crate::i18n::text("No eligible people found. Add a contact or open a DM first.", &[])} } }
                    for (peer, name) in peers {
                        {
                            let was_queued = queued.read().contains(&peer);
                            let text = invitation(&state.read(), &channel, &peer, address.as_deref(), code.as_deref());
                            let can_send = text.is_some();
                            let tx = nostr.clone();
                            let open = nostr.clone();
                            let open_peer = peer.clone();
                            let send_peer = peer.clone();
                            let cid = channel_id;
                            let address = address.clone();
                            let code = code.clone();
                            rsx! {
                                div { key: "{peer}", class: "flex items-center gap-2 rounded p-2 bg-[var(--panel2)]",
                                    div { class: "flex-1 min-w-0", title: "{peer}",
                                        span { class: "block truncate text-sm", "{name}" }
                                        span { class: "block truncate text-[10px] text-[var(--text-dim)]", "#{crate::identity::discriminator(&peer)}" }
                                    }
                                    if was_queued {
                                        button { r#type: "button", class: "text-xs text-[var(--accent)]",
                                            onclick: move |_| { open.send(NostrCmd::Open { peer: open_peer.clone() }); on_close.call(()); },
                                            {crate::i18n::text("Open DM", &[])}
                                        }
                                    } else {
                                        button { r#type: "button", class: "text-xs text-[var(--accent)] disabled:opacity-40", disabled: !can_send,
                                            "data-voice-invite-peer": "{peer}",
                                            onclick: move |_| {
                                                if queued.peek().contains(&send_peer) { return; }
                                                let text = {
                                                    let s = state.read();
                                                    s.channels.iter().find(|c| c.id == cid).and_then(|c| invitation(&s, c, &send_peer, address.as_deref(), code.as_deref()))
                                                };
                                                if let Some(text) = text {
                                                    if tx.try_send(NostrCmd::Send { peer: send_peer.clone(), text, reply_to: None, emoji: vec![], replaces: None }) {
                                                        queued.write().insert(send_peer.clone());
                                                        error.set(false);
                                                    } else { error.set(true); }
                                                }
                                            },
                                            {crate::i18n::text("Send invitation", &[])}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                p { class: "text-[10px] text-[var(--text-dim)]", {crate::i18n::text("Open the DM to check delivery or retry. Invitations do not change channel access.", &[])} }
                if address.is_none() {
                    p { class: "text-xs text-[var(--warn)]", {crate::i18n::text("A public server address is needed to invite people outside this community.", &[])} }
                } else if code.is_none() && super::invite::can_invite(&state.read(), channel.guild_id) {
                    p { class: "text-xs text-[var(--text-dim)]", {crate::i18n::text("Fetching the invite code…", &[])} }
                }
            }
        }
    }
}
