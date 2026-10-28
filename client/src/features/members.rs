use dioxus::prelude::*;
use dioxus_grid_layout::NoDrag;

use crate::protocol::VoiceState;
use crate::protocol::{ClientMessage, Id, LevelTier, Leveling, Member, Permission};
use crate::state::{use_app_state, use_gateway};
use std::collections::HashMap;

#[derive(Clone, PartialEq)]
struct MetadataInputs {
    members: Vec<Member>,
    profiles: HashMap<String, (Option<String>, bool, bool)>,
    guilds: Vec<(Id, String, Leveling)>,
    self_pubkey: Option<String>,
}

#[derive(Clone, PartialEq)]
struct VoiceInputs {
    connected_channel: Option<Id>,
    states: Vec<crate::protocol::VoiceState>,
}

#[derive(Clone, Copy)]
struct MemberSources {
    metadata: Memo<MetadataInputs>,
    voice: Memo<VoiceInputs>,
    subtitles: Memo<HashMap<String, (String, bool)>>,
}

fn use_member_sources() -> MemberSources {
    let state = use_app_state();
    let metadata = use_memo(move || {
        let s = state.read();
        MetadataInputs {
            members: s.members.clone(),
            profiles: s
                .profiles
                .iter()
                .map(|(pk, p)| {
                    (
                        pk.clone(),
                        (p.status.clone(), p.hide_guild_owner, p.operator),
                    )
                })
                .collect(),
            guilds: s
                .guilds
                .iter()
                .map(|g| (g.id, g.owner_pubkey.clone(), s.leveling_of(g.id)))
                .collect(),
            self_pubkey: s.self_user.as_ref().map(|u| u.pubkey.clone()),
        }
    });
    let voice = use_memo(move || {
        let s = state.read();
        VoiceInputs {
            connected_channel: s.connected_voice_channel(),
            states: s.voice_states.clone(),
        }
    });
    let subtitles = use_memo(move || {
        let s = state.read();
        let mut lines: HashMap<_, _> = s
            .profiles
            .iter()
            .filter_map(|(pk, p)| {
                p.custom_status
                    .as_ref()
                    .map(|cs| (pk.clone(), (cs.clone(), false)))
            })
            .collect();
        lines.extend(s.activities.iter().map(|(pk, a)| {
            (
                pk.clone(),
                (crate::features::profiles::activity_line(a), true),
            )
        }));
        lines
    });
    let sources = MemberSources {
        metadata,
        voice,
        subtitles,
    };
    use_context_provider(move || sources);
    sources
}

#[derive(Clone, PartialEq)]
struct MemberMenu {
    guild_id: Id,
    pubkey: String,
    x: f64,
    y: f64,
    confirming: Option<ModAction>,
}

#[derive(Clone, Copy, PartialEq)]
enum ModAction {
    Kick,
    Ban,
}

fn member_menu_permissions(
    state: &crate::state::AppState,
    member: &Member,
) -> (bool, bool, bool, bool) {
    if member.bot {
        return (false, false, false, false);
    }
    let gid = member.guild_id;
    let protected = state
        .self_user
        .as_ref()
        .is_some_and(|u| u.pubkey == member.user.pubkey)
        || state
            .guilds
            .iter()
            .any(|g| g.id == gid && g.owner_pubkey == member.user.pubkey);
    (
        !protected && state.can(gid, Permission::KickMembers),
        !protected && state.can(gid, Permission::BanMembers),
        state.can(gid, Permission::ManageRoles) && (!protected || state.is_owner(gid)),
        !protected && state.can(gid, Permission::DisconnectMembers),
    )
}

fn voice_here<'a>(
    voice_states: &'a [VoiceState],
    guild: Id,
    pubkey: &str,
) -> Option<&'a VoiceState> {
    voice_states
        .iter()
        .find(|v| v.guild_id == guild && v.user_pubkey == pubkey && v.channel_id.is_some())
}

/// A role's id and name, and the online members listed under it.
type RoleRun = (Option<(Id, String)>, Vec<Member>);

/// Consecutive runs of the same top role, so `members` must already be in
/// role order. `None` is the run of members holding no role.
fn group_by_top_role(
    members: Vec<Member>,
    top: impl Fn(&Member) -> Option<(Id, String)>,
) -> Vec<RoleRun> {
    let mut groups: Vec<RoleRun> = Vec::new();
    for m in members {
        let role = top(&m);
        match groups.last_mut() {
            Some((last, run)) if last.as_ref().map(|r| r.0) == role.as_ref().map(|r| r.0) => {
                run.push(m)
            }
            _ => groups.push((role, vec![m])),
        }
    }
    groups
}

/// Consecutive runs of the same rank, so `members` must already be in rank
/// order. `None` is the run below the first rank.
fn group_by_rank(members: Vec<Member>, rules: &Leveling) -> Vec<(Option<LevelTier>, Vec<Member>)> {
    let mut groups: Vec<(Option<LevelTier>, Vec<Member>)> = Vec::new();
    for m in members {
        let tier = rules.tier_at(m.xp).cloned();
        match groups.last_mut() {
            Some((last, run)) if last.as_ref().map(|t| t.xp) == tier.as_ref().map(|t| t.xp) => {
                run.push(m)
            }
            _ => groups.push((tier, vec![m])),
        }
    }
    groups
}

#[component]
pub fn MembersPanel() -> Element {
    let state = use_app_state();
    if state.read().dm_mode {
        rsx! { DmProfilePanel {} }
    } else {
        rsx! { GuildMembersPanel {} }
    }
}

#[component]
fn DmProfilePanel() -> Element {
    let mut state = use_app_state();
    let selected = use_memo(move || state.read().preferred_dm());
    use_effect(move || {
        if let Some(dm) = selected()
            && state.peek().selected_channel != Some(dm.channel_id)
        {
            state.write().selected_channel = Some(dm.channel_id);
        }
    });
    rsx! {
        if let Some(dm) = selected() {
            for peer in std::iter::once(dm.other_pubkey) {
                super::profiles::ProfileCard { key: "{peer}", embedded: true, peer: Some(peer.clone()) }
            }
        } else {
            aside { class: "panel-hover w-full h-full bg-[var(--panel)] border border-[var(--border)] rounded-xl flex items-center justify-center p-4 text-sm text-[var(--text-muted)]",
                {crate::i18n::text("Pick a conversation", &[])}
            }
        }
    }
}

#[component]
fn GuildMembersPanel() -> Element {
    #[cfg(test)]
    tests::record_render("list");
    let state = use_app_state();
    use_member_sources();
    let roster_input = use_memo(move || {
        let s = state.read();
        let gid = s.selected_guild;
        let rules = gid.map(|g| s.leveling_of(g)).unwrap_or_default();
        let members: Vec<_> = s
            .members
            .iter()
            .filter(|m| Some(m.guild_id) == gid)
            .cloned()
            .map(|mut m| {
                if !matches!(
                    rules.member_sort,
                    crate::protocol::MemberSort::Level | crate::protocol::MemberSort::Rank
                ) {
                    m.xp = 0;
                }
                m
            })
            .collect();
        let roles = gid.map(|g| s.roles_of(g).to_vec()).unwrap_or_default();
        (gid, members, rules, roles)
    });
    let roster = use_memo(move || {
        #[cfg(test)]
        tests::record_render("sort");
        let _input = roster_input.read();
        let snapshot = state.peek();

        let guild_id = snapshot.selected_guild;
        let members: Vec<Member> = guild_id
            .map(|gid| {
                snapshot
                    .members_of(gid)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let rules = guild_id
            .map(|gid| snapshot.leveling_of(gid))
            .unwrap_or_default();
        let by_role = rules.member_sort == crate::protocol::MemberSort::Role;
        let by_rank = rules.member_sort == crate::protocol::MemberSort::Rank && rules.enabled;
        let top_roles: std::collections::HashMap<String, (Id, String)> = guild_id
            .filter(|_| by_role)
            .map(|gid| {
                members
                    .iter()
                    .filter_map(|m| {
                        let r = snapshot.top_role(gid, m)?;
                        Some((m.user.pubkey.clone(), (r.id, r.name.clone())))
                    })
                    .collect()
            })
            .unwrap_or_default();
        (guild_id, members, rules, by_role, by_rank, top_roles)
    });
    let (_, members, rules, by_role, by_rank, top_roles) = roster();
    let mut menu = use_signal::<Option<MemberMenu>>(|| None);

    let (online_members, offline_members): (Vec<Member>, Vec<Member>) =
        members.iter().cloned().partition(|m| m.online);
    // Key, label, the label's emoji, and the run.
    let online_groups: Vec<(String, String, Option<String>, Vec<Member>)> = if by_rank {
        group_by_rank(online_members.clone(), &rules)
            .into_iter()
            .map(|(tier, run)| match tier {
                Some(t) => (
                    format!("rank-{}", t.xp),
                    format!("{} — {}", t.name, run.len()),
                    Some(t.emoji),
                    run,
                ),
                None => (
                    "online".into(),
                    crate::i18n::text("Online — {0}", &[run.len().to_string()]),
                    None,
                    run,
                ),
            })
            .collect()
    } else if by_role {
        group_by_top_role(online_members.clone(), |m| {
            top_roles.get(&m.user.pubkey).cloned()
        })
        .into_iter()
        .map(|(role, run)| match role {
            Some((id, name)) => (id.to_string(), format!("{name} — {}", run.len()), None, run),
            None => (
                "online".into(),
                crate::i18n::text("Online — {0}", &[run.len().to_string()]),
                None,
                run,
            ),
        })
        .collect()
    } else if online_members.is_empty() {
        Vec::new()
    } else {
        vec![(
            "online".into(),
            crate::i18n::text("Online — {0}", &[online_members.len().to_string()]),
            None,
            online_members.clone(),
        )]
    };
    let online_count = members.iter().filter(|m| m.online).count();

    let on_context = use_callback(move |(member, x, y): (Member, f64, f64)| {
        let (kick, ban, roles, disconnect) = member_menu_permissions(&state.read(), &member);
        if !kick && !ban && !roles && !disconnect {
            return;
        }
        menu.set(Some(MemberMenu {
            guild_id: member.guild_id,
            pubkey: member.user.pubkey.clone(),
            x,
            y,
            confirming: None,
        }));
    });

    rsx! {
        aside { class: "panel-hover w-full h-full bg-[var(--panel)] border border-[var(--border)] rounded-xl flex flex-col overflow-hidden",
            div { class: "h-12 px-3.5 flex items-center border-b border-[var(--border)]",
                h2 { class: "dxf-display text-[15px] font-bold tracking-tight text-[var(--text)] truncate", {crate::i18n::text("Members", &[])} }
                span { class: "ml-auto font-mono text-[11px] text-[var(--text-dim)]",
                    "{online_count}"
                }
            }
            NoDrag {
                div { class: "flex-1 overflow-y-auto py-3 space-y-3",
                    for (key, label, emoji, run) in online_groups.iter().cloned() {
                        Section {
                            key: "{key}",
                            label,
                            emoji,
                            members: run,
                            on_context,
                        }
                    }
                    if !offline_members.is_empty() {
                        Section {
                            label: format!("Offline — {}", offline_members.len()),
                            members: offline_members.clone(),
                            on_context,
                            collapsible: true,
                        }
                    }
                    if members.is_empty() {
                        div { class: "px-4 text-xs text-[var(--text-dim)]", {crate::i18n::text("No members", &[])} }
                    }
                }
            }

            if let Some(m) = menu() {
                {
                    let access = {
                        let state = state.read();
                        state.members.iter()
                            .find(|member| member.guild_id == m.guild_id && member.user.pubkey == m.pubkey)
                            .map(|member| member_menu_permissions(&state, member))
                            .unwrap_or_default()
                    };
                    rsx! {
                        MemberMenuPopover {
                            menu: m,
                            can_kick: access.0,
                            can_ban: access.1,
                            can_roles: access.2,
                            can_disconnect: access.3,
                            on_close: move |_| menu.set(None),
                            on_confirm: move |action: ModAction| {
                                if let Some(cur) = menu.write().as_mut() {
                                    cur.confirming = Some(action);
                                }
                            },
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn MemberMenuPopover(
    menu: MemberMenu,
    can_kick: bool,
    can_ban: bool,
    can_roles: bool,
    can_disconnect: bool,
    on_close: EventHandler<()>,
    on_confirm: EventHandler<ModAction>,
) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

    let gid = menu.guild_id;
    let target_pk = menu.pubkey.clone();
    let username = state.read().display_name(&target_pk);
    let in_a_call_here = state
        .read()
        .voice_states
        .iter()
        .any(|v| v.user_pubkey == target_pk && v.guild_id == gid && v.channel_id.is_some());
    let gw_disconnect = gateway.clone();
    let pk_disconnect = menu.pubkey.clone();
    let (target_roles, guild_roles) = {
        let s = state.read();
        let assigned = s
            .members
            .iter()
            .find(|m| m.guild_id == gid && m.user.pubkey == target_pk)
            .map(|m| m.roles.clone())
            .unwrap_or_default();
        (assigned, s.roles_of(gid).to_vec())
    };

    rsx! {
        div {
            class: "fixed inset-0 z-50",
            onclick: move |_| on_close.call(()),
            oncontextmenu: move |e| { e.prevent_default(); on_close.call(()); },
            div {
                class: "dxf-pop-in absolute min-w-48 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg p-1 text-sm",
                style: "left: {menu.x}px; top: {menu.y}px;",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-3 py-1.5 text-xs text-[var(--text-muted)] border-b border-[var(--border)] mb-1 truncate",
                    "{username}"
                }
                match menu.confirming {
                    None => rsx! {
                        if can_roles && !guild_roles.is_empty() {
                            div { class: "px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--text-dim)]", {crate::i18n::text("Roles", &[])} }
                            for role in guild_roles.iter().cloned() {
                                {
                                    let has = target_roles.contains(&role.id);
                                    let gw = gateway.clone();
                                    let pk = menu.pubkey.clone();
                                    let rid = role.id;
                                    rsx! {
                                        label {
                                            key: "{rid}",
                                            class: "flex items-center gap-2 px-3 py-1 text-xs text-[var(--text)] cursor-pointer select-none hover:bg-white/[0.04] rounded",
                                            input {
                                                r#type: "checkbox",
                                                checked: has,
                                                onchange: move |_| {
                                                    let msg = if has {
                                                        ClientMessage::UnassignRole { guild_id: gid, role_id: rid, user_pubkey: pk.clone() }
                                                    } else {
                                                        ClientMessage::AssignRole { guild_id: gid, role_id: rid, user_pubkey: pk.clone() }
                                                    };
                                                    gw.send(msg);
                                                },
                                            }
                                            span {
                                                style: role.color.as_deref().map(|c| format!("color: {c};")).unwrap_or_default(),
                                                "{role.name}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if can_disconnect && in_a_call_here {
                            button {
                                class: "w-full text-left px-3 py-1.5 rounded text-[var(--warn)] hover:bg-[var(--warn)]/10 transition-colors",
                                title: crate::i18n::text("Drops their voice, screen share and camera. They can join again.", &[]),
                                onclick: move |_| {
                                    gw_disconnect.send(ClientMessage::DisconnectVoice {
                                        guild_id: gid,
                                        user_pubkey: pk_disconnect.clone(),
                                    });
                                    on_close.call(());
                                },
                                {crate::i18n::text("Disconnect from voice", &[])}
                            }
                        }
                        if can_kick {
                            button {
                                class: "w-full text-left px-3 py-1.5 rounded text-[var(--danger)] hover:bg-[var(--danger)]/10 transition-colors",
                                onclick: move |_| on_confirm.call(ModAction::Kick),
                                {crate::i18n::text("Kick", &[])}
                            }
                        }
                        if can_ban {
                            button {
                                class: "w-full text-left px-3 py-1.5 rounded text-[var(--danger)] hover:bg-[var(--danger)]/10 transition-colors",
                                onclick: move |_| on_confirm.call(ModAction::Ban),
                                {crate::i18n::text("Ban", &[])}
                            }
                        }
                    },
                    Some(action) => {
                        let (verb, msg_hint) = match action {
                            ModAction::Kick => ("Kick", "They can rejoin later."),
                            ModAction::Ban => ("Ban", "They won't be able to rejoin, even by invite."),
                        };
                        let gw = gateway.clone();
                        let pk = menu.pubkey.clone();
                        rsx! {
                            div { class: "px-3 py-1.5 text-xs text-[var(--text-muted)]",
                                "{verb} {username}? {msg_hint}"
                            }
                            div { class: "flex gap-1 px-1 pb-0.5",
                                button {
                                    class: "flex-1 px-2 py-1 rounded text-xs uppercase tracking-wider text-[var(--danger)] border border-[var(--danger)]/40 hover:bg-[var(--danger)]/10 transition-colors",
                                    onclick: move |_| {
                                        let msg = match action {
                                            ModAction::Kick => ClientMessage::KickMember { guild_id: gid, user_pubkey: pk.clone() },
                                            ModAction::Ban => ClientMessage::BanMember { guild_id: gid, user_pubkey: pk.clone() },
                                        };
                                        gw.send(msg);
                                        on_close.call(());
                                    },
                                    "{verb}"
                                }
                                button {
                                    class: "flex-1 px-2 py-1 rounded text-xs uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                                    onclick: move |_| on_close.call(()),
                                    {crate::i18n::text("Cancel", &[])}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn Section(
    label: String,
    #[props(default)] emoji: Option<String>,
    members: Vec<Member>,
    on_context: EventHandler<(Member, f64, f64)>,
    #[props(default)] collapsible: bool,
) -> Element {
    let guild_id = members.first().map(|m| m.guild_id);
    let mut open = use_signal(|| false);
    let show = !collapsible || open();

    rsx! {
        div {
            if collapsible {
                button {
                    class: "w-full px-3 pt-2 pb-1.5 flex items-center gap-1.5 text-left font-mono text-[9px] uppercase tracking-[0.18em] text-[var(--text-dim)] hover:text-[var(--text)] transition-colors",
                    onclick: move |_| open.toggle(),
                    span { class: "text-[8px]", if open() { "▾" } else { "▸" } }
                    "{label}"
                }
            } else {
                div { class: "px-3 pt-2 pb-1.5 flex items-center gap-1.5 font-mono text-[9px] uppercase tracking-[0.18em] text-[var(--text-dim)]",
                    if let Some(e) = emoji.clone() {
                        span { class: "text-[11px]", style: "text-transform: none; letter-spacing: normal;",
                            crate::features::chat::EmojiText { text: e, guild_id, compact: true }
                        }
                    }
                    "{label}"
                }
            }
            if show {
                for m in members.iter() {
                    MemberRow {
                        key: "{m.guild_id}-{m.user.pubkey}",
                        member: m.clone(),
                        on_context,
                    }
                }
            }
        }
    }
}

#[component]
fn MemberRow(member: Member, on_context: EventHandler<(Member, f64, f64)>) -> Element {
    #[cfg(test)]
    tests::record_render(&format!("row:{}", member.user.pubkey));
    let mut state = use_app_state();
    let sources = use_context::<MemberSources>();
    let metadata = use_memo(use_reactive((&member,), move |(member,)| {
        let _input = sources.metadata.read();
        let s = state.peek();
        let member = s
            .members
            .iter()
            .find(|m| m.guild_id == member.guild_id && m.user.pubkey == member.user.pubkey)
            .cloned()
            .unwrap_or(member);
        let is_self = state
            .peek()
            .self_user
            .as_ref()
            .map(|u| u.pubkey == member.user.pubkey)
            .unwrap_or(false);

        let name_class = if member.online {
            "text-[var(--text)]"
        } else {
            "text-[var(--text-dim)]"
        };
        let dim = if member.online { "" } else { "opacity-60" };

        let (dot_color, pulse) = {
            let status = state.peek().presence_of(&member.user.pubkey).to_string();
            let pulse = if status == "online" {
                "dxf-dot-pulse"
            } else {
                ""
            };
            (crate::features::profiles::status_color(&status), pulse)
        };

        // A ranked member wears the rank's emoji by their name, never its name on
        // the right; the number is only for someone below every rank.
        let level = {
            let rules = state.peek().leveling_of(member.guild_id);
            let ranked = rules.tier_at(member.xp).is_some();
            let lv = crate::protocol::level_progress(member.xp).0;
            (rules.enabled && !ranked && (!rules.tiers.is_empty() || lv > 1))
                .then(|| format!("Lv{lv}"))
        };
        let (crown, operator) = {
            let s = state.peek();
            let pk = &member.user.pubkey;
            let profile = s.profile_of(pk);
            let owner = s
                .guilds
                .iter()
                .any(|g| g.id == member.guild_id && &g.owner_pubkey == pk);
            (
                owner && !profile.is_some_and(|p| p.hide_guild_owner),
                profile.is_some_and(|p| p.operator),
            )
        };

        (
            is_self, name_class, dim, dot_color, pulse, level, crown, operator,
        )
    }));
    let (is_self, name_class, dim, dot_color, pulse, level, crown, operator) = metadata();
    let subtitle_pk = member.user.pubkey.clone();
    let subtitle = use_memo(move || sources.subtitles.read().get(&subtitle_pk).cloned());
    let voice_pk = member.user.pubkey.clone();
    let voice_guild = member.guild_id;
    let voice = use_memo(move || {
        let input = sources.voice.read();
        let vs = voice_here(&input.states, voice_guild, &voice_pk);
        let same_channel = vs
            .and_then(|v| v.channel_id)
            .is_some_and(|cid| Some(cid) == input.connected_channel);
        let ring = if same_channel {
            crate::state::talk_ring(vs.is_some_and(|v| v.speaking))
        } else {
            ""
        };
        let badges = vs.map(|v| (v.muted, v.deafened));
        (ring, badges)
    });
    let (speaking_ring, badges) = voice();
    let card_pubkey = member.user.pubkey.clone();
    let ctx_member = member.clone();
    let name_pk = member.user.pubkey.clone();
    let name = use_memo(move || state.read().display_name(&name_pk))();
    rsx! {
        div {
            class: "flex items-center gap-2.5 h-9 px-2.5 mx-1 rounded-lg hover:bg-[var(--panel2)] cursor-pointer",
            title: if is_self { crate::i18n::text("Click to view your profile", &[]) } else { crate::i18n::text("Click to view profile", &[]) },
            onclick: move |_| state.write().profile_card = Some(card_pubkey.clone()),
            oncontextmenu: move |e: MouseEvent| {
                e.prevent_default();
                let c = e.client_coordinates();
                on_context.call((ctx_member.clone(), c.x, c.y));
            },
            div { class: "relative shrink-0",
                crate::features::profiles::Avatar {
                    pubkey: member.user.pubkey.clone(),
                    name: name.clone(),
                    size: "w-7 h-7 {dim} {speaking_ring}",
                }
                span {
                    class: "absolute -bottom-0.5 -right-0.5 w-2.5 h-2.5 rounded-full border-2 border-[var(--panel-solid)] {pulse}",
                    style: "background-color:{dot_color}; color:{dot_color};",
                }
            }
            div { class: "flex flex-col min-w-0 flex-1",
                span {
                    class: "text-sm truncate flex items-center gap-1.5 {name_class}",
                    title: "{member.user.pubkey}",
                    span { class: "truncate", "{name}" }
                    if crown {
                        span {
                            class: "dxf-badge block w-3 h-3 shrink-0",
                            style: "color:var(--text-muted);",
                            title: crate::i18n::text("Guild owner", &[]),
                            dangerous_inner_html: crate::features::icons::CROWN,
                        }
                    }
                    if operator {
                        span {
                            class: "dxf-badge block w-3 h-3 shrink-0",
                            style: "color:var(--text-muted);",
                            title: crate::i18n::text("Runs this server", &[]),
                            dangerous_inner_html: crate::features::icons::SCREEN,
                        }
                    }
                    if member.bot {
                        span {
                            class: "dxf-pop px-1 py-px rounded bg-[var(--accent-soft)] text-[var(--accent)] text-[8px] font-bold uppercase tracking-wider",
                            title: crate::i18n::text("Installed bot", &[]),
                            {crate::i18n::text("Bot", &[])}
                        }
                    }
                }
                if let Some((sub, is_activity)) = subtitle() {
                    span {
                        class: if is_activity { "text-[10px] text-[var(--accent)] truncate" } else { "text-[10px] text-[var(--text-dim)] truncate" },
                        title: "{sub}", "{sub}"
                    }
                }
            }
            if let Some((muted, deafened)) = badges && (muted || deafened) {
                VoiceBadges { muted, deafened }
            }
            // Lv1 is where everyone starts, so on most rows the badge repeats a
            // value that separates nobody from nobody. A named rank is worth
            // drawing from the first one, because the guild chose to name it.
            if let Some(level) = level {
                span {
                    class: "text-[10px] font-semibold shrink-0 text-[var(--text-dim)]",
                    "{level}"
                }
            }
        }
    }
}

#[component]
fn VoiceBadges(muted: bool, deafened: bool) -> Element {
    rsx! {
        div { class: "flex items-center gap-1 shrink-0 text-[var(--text-dim)]",
            if deafened {
                span {
                    class: "block w-3 h-3",
                    title: crate::i18n::text("Deafened", &[]),
                    dangerous_inner_html: crate::features::icons::HEADPHONES_OFF,
                }
            } else if muted {
                span {
                    class: "block w-3 h-3",
                    title: crate::i18n::text("Muted", &[]),
                    dangerous_inner_html: crate::features::icons::MIC_OFF,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn owner_has_every_permission_and_can_edit_own_roles_without_self_moderation() {
        let mut state = AppState::empty();
        let gid = Id::new_v4();
        let owner = member("owner", gid, true);
        state.self_user = Some(owner.user.clone());
        state.guilds.push(
            serde_json::from_value(serde_json::json!({
                "id": gid, "name": "Community", "icon": null, "owner_pubkey": "owner"
            }))
            .unwrap(),
        );
        for permission in Permission::ALL {
            assert!(state.can(gid, *permission), "missing {permission:?}");
        }
        assert_eq!(
            member_menu_permissions(&state, &owner),
            (false, false, true, false)
        );
        let other = member("other", gid, true);
        assert_eq!(
            member_menu_permissions(&state, &other),
            (true, true, true, true)
        );
        let role = Id::new_v4();
        state.self_user = Some(other.user.clone());
        let mut moderator = other.clone();
        moderator.roles.push(role);
        state.members.push(moderator.clone());
        state.roles.insert(
            gid,
            vec![crate::protocol::Role {
                id: role,
                guild_id: gid,
                name: "Moderator".into(),
                color: None,
                permissions: vec![
                    Permission::ManageRoles,
                    Permission::KickMembers,
                    Permission::MoveMembers,
                ],
                position: 0,
            }],
        );
        assert_eq!(
            member_menu_permissions(&state, &owner),
            (false, false, false, false)
        );
        assert_eq!(
            member_menu_permissions(&state, &moderator),
            (false, false, false, false)
        );
        assert!(!state.can(gid, Permission::ManageGuild));
        assert!(state.can(gid, Permission::MoveMembers));
        assert_eq!(
            member_menu_permissions(&state, &member("regular", gid, true)),
            (true, false, true, false)
        );
    }

    #[test]
    fn dm_panel_renders_the_peer_profile_without_sorting_guild_members() {
        use std::{cell::RefCell, rc::Rc};
        type Export = Rc<RefCell<Option<Signal<AppState>>>>;
        let export: Export = Rc::new(RefCell::new(None));
        RENDERS.with_borrow_mut(|counts| counts.clear());
        let mut dom = VirtualDom::new_with_props(
            |export: Export| {
                let state = use_signal(|| {
                    let mut state = AppState::empty();
                    state.dm_mode = true;
                    let peer = "11".repeat(32);
                    let channel_id = Id::new_v4();
                    state.dms.push(crate::state::DmInfo {
                        channel_id,
                        other_pubkey: peer.clone(),
                    });
                    state.dm_accepted.insert(peer.clone());
                    state.nostr_names.insert(peer, ("DM peer".into(), 0));
                    state
                });
                provide_context(state);
                provide_context(crate::nostr::service::NostrTx::test_channel());
                *export.borrow_mut() = Some(state);
                rsx! { MembersPanel {} }
            },
            export.clone(),
        );
        let edits = dom.rebuild_to_vec().edits;
        assert!(edits.iter().any(|edit| matches!(edit,
            dioxus::core::Mutation::CreateTextNode { value, .. } if *value == "DM peer"
        )));
        assert!(export.borrow().unwrap().read().profile_card.is_none());
        let mut state = export.borrow().unwrap();
        let next = Id::new_v4();
        {
            let mut state = state.write();
            let peer = "22".repeat(32);
            state.dms.push(crate::state::DmInfo {
                channel_id: next,
                other_pubkey: peer.clone(),
            });
            state
                .nostr_names
                .insert(peer.clone(), ("Another peer".into(), 0));
            state.dm_accepted.insert(peer);
            state.selected_channel = Some(next);
        }
        let changes = dom.render_immediate_to_vec().edits;
        assert!(changes.iter().any(|edit| matches!(edit,
            dioxus::core::Mutation::CreateTextNode { value, .. } if *value == "Another peer"
        )));
        RENDERS.with_borrow(|counts| assert_eq!(counts.get("sort").copied().unwrap_or(0), 0));
    }
    use super::*;
    use crate::protocol::User;
    use crate::state::AppState;

    thread_local! {
        static RENDERS: std::cell::RefCell<std::collections::HashMap<String, usize>> = Default::default();
    }

    pub(super) fn record_render(part: &str) {
        RENDERS.with_borrow_mut(|counts| *counts.entry(part.into()).or_default() += 1);
    }

    #[test]
    fn member_rows_hide_connection_icons_and_channel_names_but_keep_speech_indication() {
        use dioxus::core::{AttributeValue, Mutation};
        use std::{cell::RefCell, rc::Rc};
        type Export = Rc<RefCell<Option<Signal<AppState>>>>;
        let export: Export = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new_with_props(
            |export: Export| {
                let state = use_signal(|| {
                    let mut s = AppState::empty();
                    s.selected_guild = Some(Id::nil());
                    s.members = vec![
                        member("alice", Id::nil(), true),
                        member("bob", Id::nil(), true),
                    ];
                    let channel = Id::new_v4();
                    s.channels.push(serde_json::from_value(serde_json::json!({
                        "id": channel, "guild_id": Id::nil(), "name": "Private voice destination", "kind": "voice"
                    })).unwrap());
                    s.voice.channel_id = Some(channel);
                    s.voice.phase = crate::state::VoicePhase::Connected;
                    s.voice_states = vec![
                        voice("alice", Id::nil(), Some(channel)),
                        voice("bob", Id::nil(), Some(Id::new_v4())),
                    ];
                    s.voice_quality
                        .insert("alice".into(), crate::state::ConnectionHealth::Poor);
                    s.voice_quality
                        .insert("bob".into(), crate::state::ConnectionHealth::Lost);
                    s
                });
                provide_context(state);
                *export.borrow_mut() = Some(state);
                rsx! { MembersPanel {} }
            },
            export.clone(),
        );
        let has_icon = |edits: &[Mutation], icon: &str| {
            edits.iter().any(|edit| matches!(edit,
            Mutation::SetAttribute { name: "dangerous_inner_html", value: AttributeValue::Text(value), .. }
                if *value == icon
        ))
        };
        let initial = dom.rebuild_to_vec().edits;
        for icon in [
            super::super::icons::SIGNAL_GOOD,
            super::super::icons::SIGNAL_POOR,
            super::super::icons::SIGNAL_LOST,
            super::super::icons::SPEAKER,
        ] {
            assert!(!has_icon(&initial, icon));
        }
        assert!(!initial.iter().any(|edit| matches!(edit,
            Mutation::SetAttribute { name: "title", value: AttributeValue::Text(value), .. }
                if value.contains("Private voice destination")
        )));
        let mut state = export.borrow().unwrap();
        state
            .write()
            .voice_quality
            .insert("bob".into(), crate::state::ConnectionHealth::Poor);
        assert!(dom.render_immediate_to_vec().edits.is_empty());
        state
            .write()
            .voice_quality
            .insert("alice".into(), crate::state::ConnectionHealth::Lost);
        assert!(dom.render_immediate_to_vec().edits.is_empty());
        state.write().voice_states[0].speaking = true;
        assert!(
            dom.render_immediate_to_vec()
                .edits
                .iter()
                .any(|edit| matches!(edit,
                    Mutation::SetAttribute { name: "class", value: AttributeValue::Text(value), .. }
                        if value.contains("ring-2 ring-[var(--success)]")
                ))
        );
        state
            .write()
            .voice_quality
            .insert("alice".into(), crate::state::ConnectionHealth::Good);
        assert!(dom.render_immediate_to_vec().edits.is_empty());
        state.write().voice.phase = crate::state::VoicePhase::Idle;
        assert!(!dom.render_immediate_to_vec().edits.is_empty());
        state
            .write()
            .voice_quality
            .insert("alice".into(), crate::state::ConnectionHealth::Poor);
        assert!(dom.render_immediate_to_vec().edits.is_empty());
    }

    #[tokio::test]
    async fn selective_member_updates() {
        use std::{cell::RefCell, rc::Rc};
        type Export = Rc<RefCell<Option<Signal<AppState>>>>;
        let exported: Export = Rc::new(RefCell::new(None));
        RENDERS.with_borrow_mut(|counts| counts.clear());
        let guild = Id::nil();
        let mut dom = VirtualDom::new_with_props(
            |export: Export| {
                let state = use_signal(|| {
                    let mut s = AppState::empty();
                    s.selected_guild = Some(Id::nil());
                    s.members = vec![
                        member("alice", Id::nil(), true),
                        member("bob", Id::nil(), true),
                    ];
                    s
                });
                provide_context(state);
                *export.borrow_mut() = Some(state);
                rsx! { MembersPanel {} }
            },
            exported.clone(),
        );
        dom.rebuild_in_place();
        dom.render_immediate_to_vec();
        let mut state = exported.borrow().unwrap();
        for i in 0..100 {
            let mut s = state.write();
            s.messages.insert(Id::new_v4(), Vec::new());
            s.voice.speaking = i % 2 == 0;
            drop(s);
            dom.render_immediate_to_vec();
        }
        RENDERS.with_borrow(|counts| {
            println!("unrelated updates: {counts:?}");
            assert!(counts.values().all(|n| *n == 1));
        });
        let renders = || RENDERS.with_borrow(Clone::clone);
        let before = renders();
        state.write().activities.insert(
            "alice".into(),
            crate::protocol::Activity {
                kind: crate::protocol::ActivityKind::Playing,
                name: "Factorio".into(),
                details: None,
                state: None,
                started_ms: Some(1000),
                image: None,
            },
        );
        dom.render_immediate_to_vec();
        let mut expected = before;
        *expected.get_mut("row:alice").unwrap() += 1;
        assert_eq!(renders(), expected);
        state.write().activities.get_mut("alice").unwrap().details = Some("New map".into());
        dom.render_immediate_to_vec();
        assert_eq!(renders(), expected);
        state.write().activities.remove("alice");
        dom.render_immediate_to_vec();
        *expected.get_mut("row:alice").unwrap() += 1;
        assert_eq!(renders(), expected);

        let channel = Id::new_v4();
        state
            .write()
            .voice_states
            .push(voice("alice", guild, Some(channel)));
        dom.render_immediate_to_vec();
        *expected.get_mut("row:alice").unwrap() += 1;
        assert_eq!(renders(), expected);
        state.write().voice_states[0].speaking = true;
        dom.render_immediate_to_vec();
        assert_eq!(renders(), expected);
        {
            let mut s = state.write();
            s.voice.channel_id = Some(channel);
            s.voice.phase = crate::state::VoicePhase::Connected;
        }
        let edits = dom.render_immediate_to_vec().edits;
        assert!(edits.iter().any(|edit| matches!(edit,
            dioxus::core::Mutation::SetAttribute { name: "class", value: dioxus::core::AttributeValue::Text(value), .. }
                if value.contains("ring-2 ring-[var(--success)]")
        )));
        *expected.get_mut("row:alice").unwrap() += 1;
        assert_eq!(renders(), expected);
        state.write().voice.phase = crate::state::VoicePhase::Idle;
        dom.render_immediate_to_vec();
        *expected.get_mut("row:alice").unwrap() += 1;
        assert_eq!(renders(), expected);
        state.write().voice_states[0].muted = true;
        dom.render_immediate_to_vec();
        *expected.get_mut("row:alice").unwrap() += 1;
        assert_eq!(renders(), expected);

        state.write().members[0].xp = 10_000;
        dom.render_immediate_to_vec();
        *expected.get_mut("row:alice").unwrap() += 1;
        assert_eq!(renders(), expected);
        state.write().profiles.insert(
            "alice".into(),
            crate::protocol::Profile {
                pubkey: "alice".into(),
                status: Some("dnd".into()),
                custom_status: Some("Busy".into()),
                ..Default::default()
            },
        );
        assert!(!dom.render_immediate_to_vec().edits.is_empty());
        *expected.get_mut("row:alice").unwrap() += 1;
        assert_eq!(renders(), expected);
        state.write().voice_states.clear();
        assert!(!dom.render_immediate_to_vec().edits.is_empty());
        *expected.get_mut("row:alice").unwrap() += 1;
        assert_eq!(renders(), expected);
        assert!(state.write().note_name("alice", "Zoe".into(), 200));
        assert!(!dom.render_immediate_to_vec().edits.is_empty());
        assert_eq!(renders()["list"], 2);
        assert_eq!(renders()["row:bob"], 1);
        assert!(!state.write().note_name("alice", "Alice".into(), 100));
        assert!(dom.render_immediate_to_vec().edits.is_empty());
        assert_eq!(state.read().selected_guild, Some(guild));
        state.write().selected_guild = Some(Id::new_v4());
        dom.render_immediate_to_vec();
        assert_eq!(renders()["list"], 3);
    }

    fn member(pubkey: &str, guild: Id, online: bool) -> Member {
        Member {
            user: User {
                pubkey: pubkey.into(),
                username: pubkey.into(),
            },
            guild_id: guild,
            online,
            bot: false,
            roles: Vec::new(),
            xp: 0,
            look: Default::default(),
        }
    }

    fn voice(pubkey: &str, guild: Id, channel: Option<Id>) -> VoiceState {
        VoiceState {
            user_pubkey: pubkey.into(),
            guild_id: guild,
            channel_id: channel,
            muted: false,
            deafened: false,
            speaking: false,
            camera_on: false,
            screen_sharing: false,
            screen_watching: Vec::new(),
        }
    }

    #[test]
    fn online_members_are_grouped_under_their_top_role_in_order() {
        let gid = Id::nil();
        let mods = (Id::from_u128(1), "Mods".to_string());
        let vips = (Id::from_u128(2), "VIPs".to_string());
        let sorted = vec![
            member("a", gid, true),
            member("b", gid, true),
            member("c", gid, true),
            member("d", gid, true),
        ];
        let top = |m: &Member| match m.user.pubkey.as_str() {
            "a" | "b" => Some(mods.clone()),
            "c" => Some(vips.clone()),
            _ => None,
        };
        let groups = group_by_top_role(sorted, top);
        let shape: Vec<(Option<String>, Vec<&str>)> = groups
            .iter()
            .map(|(r, run)| {
                (
                    r.as_ref().map(|r| r.1.clone()),
                    run.iter().map(|m| m.user.pubkey.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (Some("Mods".to_string()), vec!["a", "b"]),
                (Some("VIPs".to_string()), vec!["c"]),
                (None, vec!["d"]),
            ]
        );
    }

    #[test]
    fn online_members_are_grouped_under_their_rank() {
        let gid = Id::nil();
        let tier = |xp: u64, name: &str| LevelTier {
            xp,
            name: name.into(),
            color: None,
            emoji: "⭐".into(),
        };
        let rules = Leveling {
            tiers: vec![tier(10, "Regular"), tier(100, "Veteran")],
            ..Leveling::default()
        };
        let at = |pk: &str, xp: u64| Member {
            xp,
            look: Default::default(),
            ..member(pk, gid, true)
        };
        let sorted = vec![at("a", 500), at("b", 100), at("c", 20), at("d", 3)];
        let shape: Vec<(Option<String>, Vec<String>)> = group_by_rank(sorted, &rules)
            .into_iter()
            .map(|(t, run)| {
                (
                    t.map(|t| t.name),
                    run.into_iter().map(|m| m.user.pubkey).collect(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (Some("Veteran".into()), vec!["a".to_string(), "b".into()]),
                (Some("Regular".into()), vec!["c".to_string()]),
                (None, vec!["d".to_string()]),
            ]
        );
    }

    /// Someone in voice in another guild is online here, not "in voice" here —
    /// and must not fall out of every bucket on the way.
    #[test]
    fn a_call_in_another_guild_is_not_a_call_here() {
        let here = Id::new_v4();
        let elsewhere = Id::new_v4();
        let states = [
            voice("a", elsewhere, Some(Id::new_v4())),
            voice("b", here, Some(Id::new_v4())),
            voice("c", here, None),
        ];
        assert!(voice_here(&states, here, "a").is_none());
        assert_eq!(voice_here(&states, here, "b"), Some(&states[1]));
        assert!(voice_here(&states, here, "c").is_none());
    }
}
