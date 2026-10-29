use std::collections::HashMap;

use dioxus::prelude::*;
use futures_util::StreamExt;

use crate::known_servers::{self, KnownGuild, KnownServer};
use crate::net::Reach;
use crate::protocol::Id;
use crate::state::{AppState, ConnectionStatus, SessionMode, SessionParams, use_app_state};

#[derive(Debug, Clone, PartialEq)]
pub enum Probe {
    Checking,
    Done(Reach),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Asking the server before anything here is torn down.
    Checking,
    /// A call or a hosted server would be lost; the person decides.
    Confirm,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Switch {
    pub server: KnownServer,
    pub guild: Option<Id>,
    pub voice_invite: Option<crate::state::PendingVoiceInvite>,
    stage: Stage,
}

/// The servers beside the connected one. Lives in `WorkspaceView`, so it is
/// re-read from disk on every session and never outlives one.
#[derive(Clone, Copy)]
pub struct ServerRail {
    pub known: Signal<Vec<KnownServer>>,
    pub reach: Signal<HashMap<String, Probe>>,
    pub switch: Signal<Option<Switch>>,
    pub adding: Signal<bool>,
    current: Signal<SessionMode>,
    checks: Coroutine<KnownServer>,
}

impl ServerRail {
    pub fn current_key(&self) -> String {
        known_servers::server_key(&self.current.read())
    }

    /// Listed once it has been Ready; before that, only what was dialled.
    pub fn current_server(&self) -> KnownServer {
        let key = self.current_key();
        self.known
            .read()
            .iter()
            .find(|s| s.key() == key)
            .cloned()
            .unwrap_or_else(|| KnownServer::new(self.current.read().clone()))
    }

    pub fn check(&self, server: KnownServer) {
        self.checks.send(server);
    }

    pub fn remove(mut self, key: &str) {
        let mut list = self.known.write();
        known_servers::remove(&mut list, key);
        known_servers::save(&list);
        self.reach.write().remove(key);
    }

    /// A second click on a server already being asked is the same request.
    pub fn request_switch(mut self, server: KnownServer, guild: Option<Id>) {
        self.request_switch_with_invite(server, guild, None);
    }

    pub fn request_voice_invite(
        mut self,
        server: KnownServer,
        invite: crate::state::PendingVoiceInvite,
    ) {
        self.request_switch_with_invite(server, Some(invite.guild_id), Some(invite));
    }

    fn request_switch_with_invite(
        &mut self,
        server: KnownServer,
        guild: Option<Id>,
        voice_invite: Option<crate::state::PendingVoiceInvite>,
    ) {
        let busy = self.switch.peek().as_ref().is_some_and(|s| {
            s.server.key() == server.key() && s.guild == guild && s.voice_invite == voice_invite
        });
        if !busy {
            self.switch.set(Some(Switch {
                server,
                guild,
                voice_invite,
                stage: Stage::Checking,
            }));
        }
    }

    pub fn switching_to(&self) -> Option<String> {
        self.switch.read().as_ref().map(|s| s.server.key())
    }
}

pub fn use_server_rail(params: &SessionParams, state: Signal<AppState>) -> ServerRail {
    let mode = params.mode.clone();
    let mut known = use_signal(known_servers::load);
    let mut reach = use_signal(HashMap::<String, Probe>::new);
    let switch = use_signal(|| None::<Switch>);
    let adding = use_signal(|| false);
    let current = use_signal(|| mode.clone());

    let checks = use_coroutine(move |mut asks: UnboundedReceiver<KnownServer>| async move {
        let mut running = futures_util::stream::FuturesUnordered::new();
        loop {
            tokio::select! {
                ask = asks.next() => {
                    let Some(server) = ask else { break };
                    reach.write().insert(server.key(), Probe::Checking);
                    running.push(async move {
                        let answer = crate::net::probe(&server.mode).await;
                        (server.key(), answer)
                    });
                }
                Some((key, answer)) = running.next(), if !running.is_empty() => {
                    learn_name(known, &key, &answer);
                    reach.write().insert(key, Probe::Done(answer));
                }
            }
        }
    });

    let rail = ServerRail {
        known,
        reach,
        switch,
        adding,
        current,
        checks,
    };

    use_hook(move || {
        let here = known_servers::server_key(&current.peek());
        for server in known.peek().iter() {
            let named = server.name.is_some() || !matches!(server.mode, SessionMode::ByCode { .. });
            if server.key() != here || !named {
                rail.check(server.clone());
            }
        }
    });

    // Only a Ready session says what the server's guilds are.
    let seen = use_memo(move || {
        let s = state.read();
        (s.status == ConnectionStatus::Ready).then(|| {
            s.guilds
                .iter()
                .map(|g| KnownGuild {
                    id: g.id,
                    name: g.name.clone(),
                    icon: g.icon.clone(),
                })
                .collect::<Vec<_>>()
        })
    });
    use_effect(move || {
        let Some(guilds) = seen() else { return };
        let mut list = known.write();
        known_servers::upsert(&mut list, mode.clone(), Some(guilds));
        known_servers::save(&list);
    });

    rail
}

fn learn_name(mut known: Signal<Vec<KnownServer>>, key: &str, answer: &Reach) {
    let Reach::Up { name: Some(name) } = answer else {
        return;
    };
    let stale = known
        .peek()
        .iter()
        .any(|s| s.key() == key && s.name.as_ref() != Some(name));
    if stale {
        let mut list = known.write();
        if let Some(server) = list.iter_mut().find(|s| s.key() == key) {
            server.name = Some(name.clone());
        }
        known_servers::save(&list);
    }
}

/// Where the rail stands on one server, for its dot and its tooltip.
struct Status {
    color: &'static str,
    hollow: bool,
    pulse: bool,
    text: String,
}

fn status_of(rail: &ServerRail, server: &KnownServer, current: Option<ConnectionStatus>) -> Status {
    let dot = |color, text: &str| Status {
        color,
        hollow: false,
        pulse: false,
        text: crate::i18n::message(text),
    };
    if let Some(status) = current {
        return match status {
            ConnectionStatus::Ready => dot("var(--up)", "Connected"),
            ConnectionStatus::Reconnecting => Status {
                pulse: true,
                ..dot("var(--warn)", "Reconnecting")
            },
            _ => Status {
                pulse: true,
                ..dot("var(--warn)", "Connecting")
            },
        };
    }
    match rail.reach.read().get(&server.key()) {
        Some(Probe::Checking) => Status {
            pulse: true,
            ..dot("var(--text-dim)", "Checking…")
        },
        Some(Probe::Done(Reach::Up { .. })) => dot("var(--up)", "Up — click a guild to switch"),
        Some(Probe::Done(Reach::Down(why))) => {
            dot("var(--danger)", &format!("Not answering: {why}"))
        }
        Some(Probe::Done(Reach::Local)) => Status {
            hollow: true,
            ..dot(
                "var(--text-dim)",
                "Yours — opening it starts it on this machine",
            )
        },
        None => dot("var(--text-dim)", "Not checked yet"),
    }
}

/// The strip above a server's guilds: its state, its name, and on hover the
/// two things you can do to it from here.
#[component]
pub fn ServerHeader(server: KnownServer, current: Option<ConnectionStatus>) -> Element {
    let rail = use_context::<ServerRail>();
    let status = status_of(&rail, &server, current);
    let label = server.label();
    let route = server.route();
    let mut tip_at = use_signal(|| (0.0f64, 0.0f64));
    let dot_style = if status.hollow {
        format!("border: 1.5px solid {};", status.color)
    } else {
        format!("background-color: {};", status.color)
    };
    let pulse = if status.pulse { "dxf-pending" } else { "" };
    let other = current.is_none();
    let to_check = server.clone();
    let key = server.key();
    let action = "w-3.5 h-3.5 shrink-0 flex items-center justify-center rounded text-[10px] leading-none text-[var(--text-dim)] opacity-0 group-hover:opacity-100 transition-opacity";

    rsx! {
        div {
            class: "dxf-server-header dxf-tip-host dxf-tip-quick group relative w-full flex items-center gap-0.5 px-1 h-4",
            onmouseenter: move |e: MouseEvent| {
                let c = e.client_coordinates();
                tip_at.set((c.x + 18.0, c.y));
            },
            span {
                class: "dxf-tip",
                style: "left: {tip_at().0}px; top: {tip_at().1}px;",
                "{label} · {status.text} · {route}"
            }
            if other {
                button {
                    r#type: "button",
                    class: "{action} hover:text-[var(--accent)]",
                    title: crate::i18n::text("Check whether it is up", &[]),
                    aria_label: crate::i18n::text("Check whether {0} is up", std::slice::from_ref(&(label))),
                    onclick: move |_| rail.check(to_check.clone()),
                    "\u{21bb}"
                }
            }
            span { class: "flex-1 min-w-0 flex items-center justify-center gap-1",
                span { class: "w-1.5 h-1.5 shrink-0 rounded-full {pulse}", style: "{dot_style}" }
                span { class: "min-w-0 truncate text-[9px] uppercase tracking-wider text-[var(--text-dim)]",
                    "{label}"
                }
            }
            if other {
                button {
                    r#type: "button",
                    class: "{action} hover:text-[var(--danger)]",
                    title: crate::i18n::text("Remove from this list", &[]),
                    aria_label: crate::i18n::text("Remove {0} from this list", std::slice::from_ref(&(label))),
                    onclick: move |_| rail.remove(&key),
                    "\u{2715}"
                }
            }
        }
    }
}

/// A server this client is not on: its guilds as they were last seen, each a
/// way to go there.
#[component]
pub fn OtherServer(server: KnownServer) -> Element {
    let rail = use_context::<ServerRail>();
    let label = server.label();
    let pending = rail.switching_to() == Some(server.key());
    let tiles: Vec<(Option<Id>, String, String, String)> = if server.guilds.is_empty() {
        vec![(
            None,
            initials(&label),
            format!("{label} — click to connect"),
            crate::identity::signature_accent(&server.key()),
        )]
    } else {
        server
            .guilds
            .iter()
            .map(|g| {
                (
                    Some(g.id),
                    g.icon.clone().unwrap_or_else(|| initials(&g.name)),
                    format!("{} — on {label}, click to switch", g.name),
                    crate::identity::signature_accent(&g.id.to_string()),
                )
            })
            .collect()
    };

    rsx! {
        ServerHeader { server: server.clone(), current: None::<ConnectionStatus> }
        for (guild, mark, tip, tint) in tiles {
            {
                let target = server.clone();
                rsx! {
                    OtherGuildTile {
                        key: "{server.key()}-{guild:?}",
                        mark,
                        tip,
                        tint,
                        pending,
                        on_click: move |_| rail.request_switch(target.clone(), guild),
                    }
                }
            }
        }
    }
}

#[component]
fn OtherGuildTile(
    mark: String,
    tip: String,
    tint: String,
    pending: bool,
    on_click: EventHandler<()>,
) -> Element {
    let mut tip_at = use_signal(|| (0.0f64, 0.0f64));
    let pulse = if pending { "dxf-pending" } else { "" };
    rsx! {
        div { class: "relative group dxf-tip-host",
            span {
                class: "dxf-tip",
                style: "left: {tip_at().0}px; top: {tip_at().1}px;",
                if pending { {crate::i18n::text("Checking it is up…", &[])} } else { "{tip}" }
            }
            button {
                r#type: "button",
                class: "w-11 h-11 rounded-2xl border border-dashed border-[var(--border)] hover:border-[var(--border-strong)] flex items-center justify-center text-xs font-medium opacity-60 group-hover:opacity-100 transition-opacity overflow-hidden {pulse}",
                style: "color: {tint}; background: color-mix(in srgb, {tint} 10%, transparent);",
                onmouseenter: move |e: MouseEvent| {
                    let c = e.client_coordinates();
                    tip_at.set((c.x + 18.0, c.y));
                },
                onclick: move |_| on_click.call(()),
                "{mark}"
            }
        }
    }
}

#[component]
pub fn AddServerButton() -> Element {
    let mut rail = use_context::<ServerRail>();
    let mut tip_at = use_signal(|| (0.0f64, 0.0f64));
    rsx! {
        div { class: "relative dxf-tip-host dxf-tip-quick",
            span {
                class: "dxf-tip",
                style: "left: {tip_at().0}px; top: {tip_at().1}px;",
                {crate::i18n::text("Add another server to this list", &[])}
            }
            button {
                r#type: "button",
                class: "dxf-add-server px-1.5 h-6 rounded-lg border border-dashed border-[var(--border)] text-[9px] whitespace-nowrap text-[var(--text-dim)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
                aria_label: crate::i18n::text("Add a server", &[]),
                onmouseenter: move |e: MouseEvent| {
                    let c = e.client_coordinates();
                    tip_at.set((c.x + 18.0, c.y));
                },
                onclick: move |_| rail.adding.set(true),
                {crate::i18n::text("+ server", &[])}
            }
        }
    }
}

fn initials(name: &str) -> String {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

/// What switching away would end, said before it does.
struct Cost {
    call: Option<String>,
    hosting: Option<usize>,
}

fn cost_of(s: &AppState) -> Cost {
    let call = s.voice.channel_id.or(s.voice.joining_channel_id).map(|id| {
        s.channels
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "voice".into())
    });
    let hosting = s
        .host_info
        .is_some()
        .then(|| super::workspace::others_connected(s));
    Cost { call, hosting }
}

/// Drives a switch from the rail: ask the server, then ask the person if
/// something would be lost, then hand it to the workspace to leave.
#[component]
pub fn SwitchHost(on_go: EventHandler<Switch>) -> Element {
    let rail = use_context::<ServerRail>();
    let mut state = use_app_state();
    let mut switch = rail.switch;

    use_effect(move || {
        let Some(asked) = switch() else { return };
        if asked.stage != Stage::Checking {
            return;
        }
        spawn(async move {
            let answer = crate::net::probe(&asked.server.mode).await;
            let key = asked.server.key();
            if switch.peek().as_ref() != Some(&asked) {
                return;
            }
            learn_name(rail.known, &key, &answer);
            let mut reach = rail.reach;
            reach.write().insert(key, Probe::Done(answer.clone()));
            if let Reach::Down(why) = answer {
                switch.set(None);
                state.write().error_toast = Some(format!(
                    "{} isn't answering ({why}). You're still here.",
                    asked.server.label()
                ));
                return;
            }
            let cost = cost_of(&state.peek());
            if cost.call.is_some() || cost.hosting.is_some() {
                switch.set(Some(Switch {
                    stage: Stage::Confirm,
                    ..asked
                }));
            } else {
                on_go.call(asked);
            }
        });
    });

    let Some(asked) = switch().filter(|s| s.stage == Stage::Confirm) else {
        return rsx! {};
    };
    let cost = cost_of(&state.read());
    let label = asked.server.label();
    let cancel = move |_| switch.set(None);
    let go = move |_| on_go.call(asked.clone());

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: cancel,
            div {
                class: "dxf-modal-in w-[26rem] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)]",
                    h3 { class: "text-sm font-medium text-[var(--warn)]", {crate::i18n::text("Switch to {0}?", std::slice::from_ref(&(label)))} }
                }
                div { class: "px-4 py-3 flex flex-col gap-2 text-sm text-[var(--text-muted)]",
                    if let Some(channel) = cost.call {
                        p { {crate::i18n::text("You'll leave the voice call in {0}, along with any screen share or camera.", std::slice::from_ref(&(channel)))} }
                    }
                    if let Some(others) = cost.hosting {
                        p {
                            {crate::i18n::text("This machine is the server you're on. Switching shuts it down", &[])}
                            match others {
                                0 => rsx! { {crate::i18n::text("; nobody else is connected right now.", &[])} },
                                1 => rsx! { {crate::i18n::text(", and the one other person connected is dropped.", &[])} },
                                n => rsx! { {crate::i18n::text(", and the {0} other people connected are dropped.", &[(n).to_string()])} },
                            }
                        }
                    }
                    p { class: "text-[var(--text-dim)]",
                        {crate::i18n::text("If {0} doesn't let you in, you come back here. Calls in DMs carry on either way.", std::slice::from_ref(&(label)))}
                    }
                }
                div { class: "px-4 py-3 border-t border-[var(--border)] flex items-center justify-end gap-2",
                    button {
                        class: "px-3 py-1 rounded text-[11px] text-[var(--text-muted)] hover:text-[var(--text)] transition-colors",
                        onclick: cancel,
                        {crate::i18n::text("Stay", &[])}
                    }
                    button {
                        class: "px-3 py-1 rounded text-[11px] uppercase tracking-wider text-[var(--warn)] border border-[var(--border)] hover:border-[var(--danger)] transition-colors",
                        onclick: go,
                        {crate::i18n::text("Switch", &[])}
                    }
                }
            }
        }
    }
}

/// A server is added by what a friend would send: a code, a name, or an
/// address. Adding only lists it; going there is a click on the rail.
#[component]
pub fn AddServerDialog() -> Element {
    let mut rail = use_context::<ServerRail>();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let mut draft = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut note = use_signal(|| None::<String>);
    if !(rail.adding)() {
        return rsx! {};
    }

    let mut close = move || {
        rail.adding.set(false);
        draft.set(String::new());
        note.set(None);
        busy.set(false);
    };
    let mut submit = move || {
        let typed = draft().trim().to_string();
        if typed.is_empty() || busy() {
            return;
        }
        let rendezvous = settings.read().rendezvous_servers.clone();
        busy.set(true);
        note.set(None);
        spawn(async move {
            let mode = match address_mode(&typed) {
                Some(mode) => mode,
                None => match crate::net::find_code(&rendezvous, &typed).await {
                    Some(rendezvous_url) => SessionMode::ByCode {
                        rendezvous_url,
                        code: typed.clone(),
                    },
                    None => {
                        busy.set(false);
                        note.set(Some(
                            "No rendezvous in your list knows that code. Check it, or add the rendezvous your friend publishes on from the home screen.".into(),
                        ));
                        return;
                    }
                },
            };
            let server = KnownServer::new(mode.clone());
            {
                let mut list = rail.known.write();
                known_servers::upsert(&mut list, mode, None);
                known_servers::save(&list);
            }
            rail.check(server);
            close();
        });
    };

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: move |_| close(),
            div {
                class: "dxf-modal-in w-[22rem] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)]",
                    h3 { class: "text-sm font-medium text-[var(--text)]", {crate::i18n::text("Add a server", &[])} }
                }
                div { class: "px-4 py-3 flex flex-col gap-2",
                    input {
                        class: "w-full bg-[var(--panel2)] border border-[var(--edge-strong)] rounded-[10px] px-3 py-2 text-[12.5px] text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors",
                        r#type: "text",
                        placeholder: crate::i18n::text("purple-fox-42, a server name, or quic://…", &[]),
                        value: "{draft}",
                        autofocus: true,
                        disabled: busy(),
                        oninput: move |e| draft.set(e.value()),
                        onkeydown: move |e| {
                            if e.key() == Key::Enter {
                                submit();
                            } else if e.key() == Key::Escape {
                                close();
                            }
                        },
                    }
                    div { class: "text-[11px] text-[var(--text-dim)] leading-relaxed",
                        if let Some(n) = note() {
                            span { class: "text-[var(--warn)]", "{n}" }
                        } else {
                            {crate::i18n::text("It joins the list beside your guilds; click it there to go. A code is looked up on every rendezvous you have.", &[])}
                        }
                    }
                }
                div { class: "px-4 py-3 border-t border-[var(--border)] flex items-center justify-end gap-2",
                    button {
                        class: "px-3 py-1 rounded text-[11px] text-[var(--text-muted)] hover:text-[var(--text)] transition-colors",
                        onclick: move |_| close(),
                        {crate::i18n::text("Cancel", &[])}
                    }
                    button {
                        class: "px-3 py-1 rounded text-[11px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors disabled:opacity-50",
                        disabled: busy() || draft().trim().is_empty(),
                        onclick: move |_| submit(),
                        if busy() { {crate::i18n::text("Looking up…", &[])} } else { {crate::i18n::text("Add", &[])} }
                    }
                }
            }
        }
    }
}

/// An address is anything with a scheme; everything else is a code or a name.
fn address_mode(typed: &str) -> Option<SessionMode> {
    typed.contains("://").then(|| SessionMode::Remote {
        server_url: typed.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_scheme_makes_an_address() {
        assert!(address_mode("quic://abc@1.2.3.4:9001").is_some());
        assert!(address_mode("wss://chat.example").is_some());
        assert!(address_mode("Friday Game Night").is_none());
        assert!(address_mode("purple-fox-42").is_none());
    }
}
