use std::collections::HashSet;

use dioxus::prelude::*;

use crate::nostr::service::{NostrCmd, NostrTx};
use crate::protocol::{Channel, ChannelKind, ClientMessage, Id, Permission};
use crate::state::{AppState, ConnectionStatus, SessionMode, use_app_state, use_gateway};

pub(super) fn can_invite_to(state: &AppState, channel: &Channel) -> bool {
    state.status == ConnectionStatus::Ready
        && channel.kind == ChannelKind::Voice
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
    if state
        .members_of(channel.guild_id)
        .into_iter()
        .any(|m| m.user.pubkey == peer)
    {
        Some(crate::i18n::text(
            "You're invited to voice channel {0} in {1} on Discordia.\nOpen {1}, then select voice channel {0}.",
            &[channel.name.clone(), guild.name.clone()],
        ))
    } else {
        Some(crate::i18n::text(
            "You're invited to voice channel {0} in {1} on Discordia.\n1. Add the server: {2}\n2. Join the community with invite code: {3}\n3. Select voice channel: {0}",
            &[
                channel.name.clone(),
                guild.name.clone(),
                address?.to_owned(),
                code?.to_owned(),
            ],
        ))
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
                expires_in_secs: None,
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
