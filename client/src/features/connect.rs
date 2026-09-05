use dioxus::prelude::*;

use crate::identity::Identity;
use crate::protocol::rendezvous::DiscoverEntry;
use crate::session::{self, SavedSession};
use crate::state::{SessionMode, SessionParams};

const HOST_STALE_AFTER_SECS: u64 = 45;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Join,
    Create,
}

fn initial_mode() -> Mode {
    Mode::Join
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JoinBy {
    Url,
    Code,
    Nothing,
}

fn join_by(server_url: &str, code: &str, rendezvous_url: &str) -> JoinBy {
    if !server_url.trim().is_empty() {
        JoinBy::Url
    } else if !code.trim().is_empty() && !rendezvous_url.trim().is_empty() {
        JoinBy::Code
    } else {
        JoinBy::Nothing
    }
}

const INPUT: &str = "w-full bg-[var(--panel2)] border border-[var(--edge-strong)] rounded-[11px] px-4 py-3.5 text-[14.5px] text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors";
const INPUT_SM: &str = "w-full bg-[var(--panel2)] border border-[var(--edge-strong)] rounded-[10px] px-3 py-2 text-[12.5px] text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors";
const LABEL: &str = "text-[11px] uppercase tracking-[0.09em] text-[var(--text-dim)]";

/// Everything about arriving at a server. A panel now, not a screen: the hero
/// and the identity card moved to `features::home`, which mounts this.
#[component]
pub fn ConnectForm(
    identity: Identity,
    error: Option<String>,
    last_session: Option<SavedSession>,
    on_connect: EventHandler<SessionParams>,
) -> Element {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let default_rendezvous = settings.read().active_rendezvous();

    let mut mode = use_signal(initial_mode);
    let mut server_url = use_signal(String::new);
    let mut allow_lan = use_signal(|| false);
    let mut publish_to_rendezvous = use_signal(|| true);
    let mut rendezvous_url = use_signal(|| default_rendezvous.clone());
    let mut code = use_signal(String::new);

    let mut publish_name = use_signal(String::new);
    let mut description = use_signal(String::new);
    let mut publish_public = use_signal(|| true);

    let identity_for_rows = identity.clone();
    let identity_for_submit = identity.clone();
    let submit = move |_| {
        let name = identity_for_submit.display_name.clone();
        let params = match mode() {
            Mode::Join => match join_by(&server_url(), &code(), &rendezvous_url()) {
                JoinBy::Url => SessionParams {
                    mode: SessionMode::Remote {
                        server_url: server_url().trim().to_string(),
                    },
                    username: name,
                    identity: identity_for_submit.clone(),
                },
                JoinBy::Code => SessionParams {
                    mode: SessionMode::ByCode {
                        rendezvous_url: rendezvous_url().trim().to_string(),
                        code: code().trim().to_string(),
                    },
                    username: name,
                    identity: identity_for_submit.clone(),
                },
                JoinBy::Nothing => return,
            },
            Mode::Create => {
                let r_url = if publish_to_rendezvous() {
                    let r = rendezvous_url().trim().to_string();
                    if r.is_empty() { None } else { Some(r) }
                } else {
                    None
                };
                let pn = publish_name().trim().to_string();
                let desc = description().trim().to_string();
                SessionParams {
                    mode: SessionMode::SelfHost {
                        allow_lan: allow_lan(),
                        rendezvous_url: r_url,
                        publish_name: if pn.is_empty() { None } else { Some(pn) },
                        description: if desc.is_empty() { None } else { Some(desc) },
                        publish_public: publish_to_rendezvous() && publish_public(),
                    },
                    username: name,
                    identity: identity_for_submit.clone(),
                }
            }
        };
        let r = rendezvous_url().trim().to_string();
        if !r.is_empty() {
            let mut next = settings.read().clone();
            next.use_rendezvous(&r);
            settings.set(next.clone());
            crate::settings::save(&next);
        }
        on_connect.call(params);
    };

    let disabled = match mode() {
        Mode::Create => false,
        Mode::Join => join_by(&server_url(), &code(), &rendezvous_url()) == JoinBy::Nothing,
    };
    // Going grey says "not yet" without saying why; this names both ways in.
    let hint = match last_session.as_ref() {
        Some(s) => format!("Enter a code above, or reconnect to {}.", session::label(s)),
        None => "Enter a code above to connect.".to_string(),
    };

    rsx! {
        div { class: "h-full w-full flex flex-col min-w-0",
            div { class: "flex-1 flex flex-col overflow-hidden min-w-0",
                form {
                    class: "flex-1 overflow-auto px-7 pt-4 pb-5 flex flex-col items-stretch dxf-no-drag",
                    onsubmit: submit,
                    div { class: "w-full flex-1 space-y-4 flex flex-col",

                if let Some(saved) = last_session.clone() {
                    {
                        let identity_for_reconnect = identity.clone();
                        let on_connect_for_reconnect = on_connect;
                        rsx! {
                            button {
                                r#type: "button",
                                class: "panel-hover w-full flex items-center justify-between gap-4 px-4 py-3.5 rounded-xl bg-[var(--panel2)] border border-[var(--border)] hover:border-[var(--accent)] text-left group",
                                onclick: move |_| {
                                    let params = SessionParams {
                                        mode: saved.mode.clone(),
                                        username: saved.username.clone(),
                                        identity: identity_for_reconnect.clone(),
                                    };
                                    on_connect_for_reconnect.call(params);
                                },
                                div { class: "flex items-center gap-3 min-w-0",
                                    div {
                                        class: "w-[34px] h-[34px] shrink-0 rounded-[9px] flex items-center justify-center",
                                        style: "background: color-mix(in srgb, var(--accent) 8%, var(--bg));",
                                        span { class: "w-2 h-2 rounded-full bg-[var(--up)]" }
                                    }
                                    div { class: "min-w-0",
                                        // Without this the label's min-content
                                        // width holds the row open and the
                                        // Reconnect pill lands on the name.
                                        div { class: "{LABEL} truncate", "Last session" }
                                        div { class: "mt-0.5 text-[15px] font-semibold text-[var(--text)] truncate",
                                            "{session::label(&saved)}"
                                        }
                                    }
                                }
                                span {
                                    class: "shrink-0 px-4 py-2.5 rounded-[9px] border text-[13px] font-semibold text-[var(--accent-strong)] group-hover:text-[var(--text)] transition-colors",
                                    style: "background: color-mix(in srgb, var(--accent) 14%, transparent); border-color: color-mix(in srgb, var(--accent) 35%, transparent);",
                                    "Reconnect →"
                                }
                            }
                        }
                    }
                }

                // Labels specify "server" to distinguish from "community"
                // (which is `CreateGuild` and requires an existing
                // connection). Bare "Join"/"Create" caused user confusion.
                div { class: "space-y-2",
                    RendezvousPicker {
                        selected: rendezvous_url(),
                        on_select: move |u: String| rendezvous_url.set(u),
                    }
                    div { class: "text-xs text-[var(--text-dim)] leading-relaxed text-pretty",
                        "Codes are looked up here, the public list comes from here, and a server of your own is published and named here."
                    }
                }

                div { class: "flex gap-1 p-1 rounded-[11px] bg-[var(--panel2)] border border-[var(--edge)]",
                    TabButton { active: mode() == Mode::Join, label: "Join a server", onclick: move |_| mode.set(Mode::Join) }
                    TabButton { active: mode() == Mode::Create, label: "Create a server", onclick: move |_| mode.set(Mode::Create) }
                }

                if let Some(err) = error {
                    div { class: "text-[12.5px] text-[var(--danger)] border border-[var(--border)] rounded-[11px] px-4 py-3",
                        "{err}"
                    }
                }

                div { class: "fade-in",
                match mode() {
                    Mode::Join => rsx! {
                        div { class: "space-y-4",
                            div { class: "space-y-2",
                                label { class: LABEL, "Code" }
                                input {
                                    class: "{INPUT} lowercase",
                                    r#type: "text",
                                    placeholder: "purple-fox-42 or a server name",
                                    value: "{code}",
                                    oninput: move |e| code.set(e.value()),
                                }
                            }

                            BrowseTab {
                                on_pick: move |entry: DiscoverEntry| {
                                    code.set(entry.shortcode);
                                },
                                on_enter: move |entry: DiscoverEntry| {
                                    let params = SessionParams {
                                        mode: SessionMode::ByCode {
                                            rendezvous_url: rendezvous_url().trim().to_string(),
                                            code: entry.shortcode,
                                        },
                                        username: identity_for_rows.display_name.clone(),
                                        identity: identity_for_rows.clone(),
                                    };
                                    on_connect.call(params);
                                },
                                picked_shortcode: code(),
                                rendezvous_url: rendezvous_url(),
                            }

                            // Folded because gateway address entry is rare and
                            // ignores the directory above. Must start empty:
                            // an open stale default looks filled, while a
                            // closed one silently dictates connections.
                            details { class: "dxf-fold",
                                summary { class: "inline-flex items-center cursor-pointer text-xs uppercase tracking-[0.06em] text-[var(--text-muted)] hover:text-[var(--text)] transition-colors",
                                    "Other ways to connect"
                                }
                                div { class: "space-y-2 mt-2.5 px-4 py-3.5 rounded-[11px] bg-[var(--panel)] border border-[var(--edge)]",
                                    label { class: LABEL, "Server address" }
                                    input {
                                        class: INPUT_SM,
                                        r#type: "text",
                                        placeholder: "quic://… from the host, or wss://…",
                                        value: "{server_url}",
                                        oninput: move |e| server_url.set(e.value()),
                                    }
                                    div { class: "text-xs text-[var(--text-dim)] leading-relaxed text-pretty",
                                        "Connects straight to a gateway, ignoring the code and the directory. A quic:// address is what a host copies from their banner; wss:// is a server behind a TLS proxy. Plain ws:// works only on this machine."
                                    }
                                }
                            }
                        }
                    },
                    Mode::Create => rsx! {
                        div { class: "rounded-[11px] bg-[var(--panel)] border border-[var(--edge)] px-4 py-3.5 text-[12.5px] space-y-3.5",
                            p { class: "text-[var(--text-muted)]",
                                "Your machine runs the server and keeps its history. Voice runs here too, unless the rendezvous above supplies its own."
                            }
                            label { class: "flex items-center gap-2 cursor-pointer text-[var(--text)]",
                                input {
                                    r#type: "checkbox",
                                    checked: publish_to_rendezvous(),
                                    oninput: move |e| publish_to_rendezvous.set(e.value() == "true"),
                                }
                                "Give it a join code friends can use"
                            }
                            div { class: "flex items-center gap-1.5",
                                label { class: "flex items-center gap-2 cursor-pointer text-[var(--text)]",
                                    input {
                                        r#type: "checkbox",
                                        checked: allow_lan(),
                                        oninput: move |e| allow_lan.set(e.value() == "true"),
                                    }
                                    "Accept direct connections"
                                }
                                span {
                                    class: "w-4 h-4 shrink-0 flex items-center justify-center rounded-full border border-[var(--border)] text-[9px] text-[var(--text-dim)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors cursor-help",
                                    title: "Offers this machine's own address in the share string and asks your router (UPnP / NAT-PMP) to let in friends elsewhere. Connections stay encrypted either way; your home IP becomes visible to anyone who joins that way.",
                                    "?"
                                }
                            }
                            if publish_to_rendezvous() {
                                div { class: "pl-3 border-l border-[var(--border)] space-y-2",
                                    div { class: "space-y-1",
                                        label { class: LABEL, "Server name" }
                                        input {
                                            class: "{INPUT_SM} lowercase",
                                            r#type: "text",
                                            placeholder: "my-server",
                                            value: "{publish_name}",
                                            oninput: move |e| publish_name.set(e.value()),
                                        }
                                        div { class: "text-[10px] text-[var(--text-dim)]",
                                            "Becomes your join code, reserved to your key. Letters, digits, '-', '_' and '.'"
                                        }
                                    }
                                    div { class: "space-y-1",
                                        label { class: LABEL, "Description (optional)" }
                                        input {
                                            class: INPUT_SM,
                                            r#type: "text",
                                            placeholder: "Friends-only chat",
                                            value: "{description}",
                                            oninput: move |e| description.set(e.value()),
                                        }
                                    }
                                    label { class: "flex items-center gap-2 cursor-pointer text-[var(--text)]",
                                        input {
                                            r#type: "checkbox",
                                            checked: publish_public(),
                                            oninput: move |e| publish_public.set(e.value() == "true"),
                                        }
                                        "List it publicly, so strangers can find it by name"
                                    }
                                }
                            }
                        }
                    },
                }
                }

                div { class: "mt-auto pt-2",
                    button {
                        class: "dxf-cta w-full py-3.5 rounded-xl transition-all disabled:cursor-not-allowed text-[15px]",
                        r#type: "submit",
                        disabled,
                        {match mode() {
                            Mode::Join => "Connect  →",
                            Mode::Create => "Launch  →",
                        }}
                    }
                    if disabled {
                        div { class: "mt-2 text-center text-xs text-[var(--text-dim)]", "{hint}" }
                    }
                }
                    }
                }
            }
        }
    }
}

#[component]
pub fn RendezvousPicker(selected: String, on_select: EventHandler<String>) -> Element {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let mut adding = use_signal(|| false);
    let mut draft = use_signal(String::new);
    let servers = settings.read().rendezvous_servers.clone();

    let mut commit = move |url: String| {
        let mut next = settings.read().clone();
        next.use_rendezvous(&url);
        settings.set(next.clone());
        crate::settings::save(&next);
        on_select.call(next.active_rendezvous());
        draft.set(String::new());
        adding.set(false);
    };

    rsx! {
        div { class: "space-y-2",
            div { class: "flex items-baseline gap-2",
                span { class: "{LABEL} flex-1", "Rendezvous server" }
                button {
                    r#type: "button",
                    class: "text-[12.5px] font-medium text-[var(--accent)] hover:text-[var(--accent-strong)] transition-colors",
                    onclick: move |_| adding.set(!adding()),
                    if adding() { "Cancel" } else { "+ Add" }
                }
            }
            if adding() {
                div { class: "flex gap-1.5",
                    input {
                        class: "{INPUT_SM} font-mono",
                        r#type: "text",
                        placeholder: "ws://192.168.0.61:7700",
                        value: "{draft}",
                        autofocus: true,
                        oninput: move |e| draft.set(e.value()),
                        onkeydown: move |e| {
                            if e.key() == Key::Enter {
                                let v = draft().trim().to_string();
                                if !v.is_empty() { commit(v); }
                            } else if e.key() == Key::Escape {
                                adding.set(false);
                            }
                        },
                    }
                    button {
                        r#type: "button",
                        class: "px-3 rounded-[10px] text-[12.5px] font-medium text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                        onclick: move |_| {
                            let v = draft().trim().to_string();
                            if !v.is_empty() { commit(v); }
                        },
                        "Save"
                    }
                }
            }
            div { class: "flex items-center gap-2",
                div { class: "flex-1 min-w-0 relative",
                    select {
                        // The native arrow is the one part of a select that no
                        // stylesheet reaches, so it is replaced rather than styled.
                        class: "w-full appearance-none rounded-[10px] border border-[var(--edge-strong)] pl-3.5 pr-9 py-2.5 font-mono text-[12.5px] focus:outline-none focus:border-[var(--accent)] transition-colors",
                        style: "color: var(--text); background: var(--panel2);",
                        onchange: move |e| on_select.call(e.value()),
                        for url in servers.iter().cloned() {
                            option {
                                key: "{url}",
                                value: "{url}",
                                selected: url == selected,
                                style: "color: var(--text); background: var(--panel-solid);",
                                "{url}"
                            }
                        }
                    }
                    span { class: "absolute right-3.5 top-1/2 -translate-y-1/2 text-[10px] text-[var(--text-dim)] pointer-events-none",
                        "▼"
                    }
                }
                button {
                    r#type: "button",
                    class: "w-[38px] h-10 shrink-0 rounded-[10px] border border-[var(--border)] flex items-center justify-center text-xs text-[var(--text-dim)] hover:text-[var(--danger)] hover:border-[var(--danger)] transition-colors",
                    title: "Forget this server",
                    onclick: move |_| {
                        let mut next = settings.read().clone();
                        next.remove_rendezvous(&selected);
                        settings.set(next.clone());
                        crate::settings::save(&next);
                        on_select.call(next.active_rendezvous());
                    },
                    "✕"
                }
            }
        }
    }
}

#[component]
fn TabButton(active: bool, label: &'static str, onclick: EventHandler<()>) -> Element {
    let cls = if active {
        "bg-[var(--accent-soft)] text-[var(--accent-strong)]"
    } else {
        "text-[var(--text-dim)] hover:text-[var(--text-muted)]"
    };
    rsx! {
        button {
            r#type: "button",
            class: "flex-1 min-w-0 px-2 py-2.5 rounded-lg text-[13.5px] font-semibold whitespace-nowrap truncate transition-colors {cls}",
            onclick: move |_| onclick.call(()),
            "{label}"
        }
    }
}

#[component]
pub fn IdentityCard(
    identity: Identity,
    on_rename: EventHandler<String>,
    on_sign_out: EventHandler<()>,
) -> Element {
    let initial = identity
        .display_name
        .chars()
        .next()
        .unwrap_or('?')
        .to_ascii_uppercase()
        .to_string();
    let tag = crate::identity::discriminator(&identity.pubkey).to_string();
    let file_path = identity.file_path_display();

    let mut editing = use_signal(|| false);
    let mut draft = use_signal(|| identity.display_name.clone());
    let signature = crate::identity::color_signature(&identity.pubkey, 16);
    let npub = identity.npub();

    rsx! {
        div { class: "rounded-2xl border border-[var(--edge)] bg-[var(--panel2)] p-4 space-y-3",
            div { class: "flex items-center gap-3 text-xs",
                div { class: "w-11 h-11 rounded-xl border border-[var(--edge)] flex items-center justify-center text-[var(--accent)] font-semibold text-lg shrink-0",
                    style: "background: var(--bg2);",
                    "{initial}"
                }
                div { class: "flex flex-col flex-1 min-w-0",
                    if editing() {
                        input {
                            class: "w-full bg-transparent border border-[var(--border)] rounded px-2 py-0.5 text-sm text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors",
                            r#type: "text",
                            value: "{draft}",
                            autofocus: true,
                            oninput: move |e| draft.set(crate::protocol::truncate_username(&e.value())),
                            onkeydown: move |e| {
                                let key = e.key().to_string();
                                if key == "Enter" {
                                    let n = draft().trim().to_string();
                                    if !n.is_empty() {
                                        on_rename.call(n);
                                    }
                                    editing.set(false);
                                } else if key == "Escape" {
                                    editing.set(false);
                                }
                            },
                        }
                    } else {
                        span {
                            class: "text-[var(--text)] truncate text-base font-semibold flex items-center gap-1.5",
                            title: "{identity.pubkey}",
                            "{identity.display_name}"
                            span { class: "text-[var(--text-dim)] font-mono text-xs font-normal",
                                "#{tag}"
                            }
                            span { class: "text-[var(--up)] text-sm", title: "Key verified", "✓" }
                        }
                    }
                    span { class: "text-[var(--text-dim)] text-[11px] font-mono select-all truncate",
                        title: "{identity.pubkey}",
                        "{npub}"
                    }
                }
                if editing() {
                    button {
                        r#type: "button",
                        class: "text-[10px] text-[var(--accent)] hover:text-[var(--accent-strong)] uppercase tracking-wider border border-[var(--edge)] rounded-md px-2.5 py-1 transition-colors",
                        onclick: move |_| {
                            let n = draft().trim().to_string();
                            if !n.is_empty() {
                                on_rename.call(n);
                            }
                            editing.set(false);
                        },
                        "save"
                    }
                } else {
                    button {
                        r#type: "button",
                        class: "text-[10px] text-[var(--text-muted)] hover:text-[var(--accent)] uppercase tracking-wider border border-[var(--edge)] rounded-md px-2.5 py-1 transition-colors",
                        onclick: move |_| {
                            draft.set(identity.display_name.clone());
                            editing.set(true);
                        },
                        title: "Rename — your pubkey stays the same",
                        "edit"
                    }
                }
                button {
                    r#type: "button",
                    class: "text-[10px] text-[var(--text-muted)] hover:text-[var(--danger)] uppercase tracking-wider border border-[var(--edge)] rounded-md px-2.5 py-1 transition-colors",
                    onclick: move |_| on_sign_out.call(()),
                    title: "Sign out. The key stays on this machine — the setup screen lists it again.",
                    "sign out"
                }
            }
            div { class: "flex gap-1",
                for c in signature.iter() {
                    div { class: "h-2 flex-1 rounded-full", style: "background: {c};" }
                }
            }
            div { class: "text-[11px] text-[var(--text-dim)]",
                "This color signature is derived from your public key. Nobody else has it."
            }
            details { class: "text-[10px] text-[var(--text-dim)]",
                summary { class: "cursor-pointer hover:text-[var(--text-muted)] transition-colors",
                    "Identity file location"
                }
                code { class: "block mt-1 text-[var(--text-muted)] font-mono break-all select-all",
                    "{file_path}"
                }
            }
        }
    }
}

/// The rendezvous drops a host at 60s, so a listed entry can already be gone;
/// saying *when* it last answered lets the reader judge that themselves.
fn freshness(idle_secs: u64) -> (String, bool) {
    match idle_secs {
        0..=25 => ("active now".to_string(), true),
        s if s < HOST_STALE_AFTER_SECS => (format!("active {s}s ago"), true),
        s if s < 3600 => (format!("quiet {}m", s / 60), false),
        s => (format!("quiet {}h", s / 3600), false),
    }
}

/// Two letters distinguish rows at a glance without inventing an avatar.
fn initials(name: &str) -> String {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

#[component]
fn BrowseTab(
    rendezvous_url: String,
    on_pick: EventHandler<DiscoverEntry>,
    on_enter: EventHandler<DiscoverEntry>,
    picked_shortcode: String,
) -> Element {
    let mut refresh_tick = use_signal(|| 0u32);
    let url_for_fetch = rendezvous_url.clone();
    let entries = use_resource(move || {
        let _ = refresh_tick();
        let base = ws_to_http(&url_for_fetch);
        async move {
            let url = format!("{base}/discover");
            reqwest::Client::new()
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("fetch: {e}"))?
                .json::<Vec<DiscoverEntry>>()
                .await
                .map_err(|e| format!("decode: {e}"))
        }
    });

    rsx! {
        div { class: "space-y-2",
            div { class: "flex items-baseline gap-2",
                span { class: "{LABEL} flex-1", "Public servers" }
                button {
                    r#type: "button",
                    class: "text-[12.5px] font-medium text-[var(--accent)] hover:text-[var(--accent-strong)] transition-colors",
                    onclick: move |_| refresh_tick.set(refresh_tick() + 1),
                    "\u{21bb} Refresh"
                }
            }

            div { class: "max-h-64 overflow-y-auto space-y-1.5 pr-0.5",
                match &*entries.read_unchecked() {
                    None => rsx! {
                        div { class: "text-[12.5px] text-[var(--text-dim)] px-6 py-5 text-center border border-[var(--edge-strong)] rounded-xl", "Loading\u{2026}" }
                    },
                    Some(Err(e)) => rsx! {
                        div { class: "text-[12.5px] px-6 py-5 space-y-1.5 border border-[var(--edge-strong)] rounded-xl",
                            div { class: "font-semibold text-[var(--danger)]", "Couldn't reach the server directory." }
                            div { class: "text-[var(--text-dim)] leading-relaxed text-pretty",
                                "A code from a friend still works, or create your own server. ({e})"
                            }
                        }
                    },
                    Some(Ok(list)) if list.is_empty() => rsx! {
                        div { class: "px-6 py-5 text-center bg-[var(--panel)] border border-dashed border-[var(--edge-strong)] rounded-xl",
                            div { class: "text-sm font-semibold text-[var(--text-muted)]", "Nothing listed here yet" }
                            div { class: "mt-1.5 mx-auto max-w-[380px] text-[12.5px] text-[var(--text-dim)] leading-relaxed text-pretty",
                                "A code from a friend works without one \u{2014} or make your own from the Create tab."
                            }
                        }
                    },
                    Some(Ok(list)) => rsx! {
                        for entry in list.iter().cloned() {
                            {
                                let sc = entry.shortcode.clone();
                                let selected = picked_shortcode == sc;
                                let title = entry.name.clone().unwrap_or_else(|| sc.clone());
                                let named = entry.name.is_some();
                                let (fresh_label, fresh_ok) = freshness(entry.idle_secs);
                                // Every host is reached over encrypted QUIC; what
                                // differs is whether it has an address of its own or
                                // only the relay to be introduced through.
                                let direct = entry
                                    .transport_addrs
                                    .iter()
                                    .any(|a| a.parse::<std::net::SocketAddr>().is_ok());
                                let stripe = if direct { "#8fb0ff" } else { "var(--violet, #b98cff)" };
                                let row_cls = if selected {
                                    "border-[var(--accent)] bg-[var(--accent-soft)]"
                                } else {
                                    "border-[var(--border)] hover:border-[var(--border-strong)]"
                                };
                                let mark = initials(&title);
                                let for_pick = entry.clone();
                                let for_enter = entry.clone();
                                rsx! {
                                    div {
                                        key: "{sc}",
                                        class: "flex items-center gap-3 pl-2.5 pr-2.5 py-2 rounded-lg border transition-colors {row_cls}",
                                        style: "border-left: 3px solid {stripe};",
                                        button {
                                            r#type: "button",
                                            class: "flex-1 min-w-0 flex items-center gap-3 text-left",
                                            onclick: move |_| on_pick.call(for_pick.clone()),
                                            span { class: "w-8 h-8 shrink-0 rounded-lg border border-[var(--border)] flex items-center justify-center text-[11px] text-[var(--text-muted)]",
                                                "{mark}"
                                            }
                                            span { class: "flex-1 min-w-0",
                                                span { class: "flex items-baseline gap-1.5",
                                                    span { class: "truncate text-[13px] text-[var(--text)]", "{title}" }
                                                    if named {
                                                        span {
                                                            class: "shrink-0 text-[9px] text-[var(--text-dim)]",
                                                            title: "A reserved name, proved with the host's key",
                                                            "\u{1f511}"
                                                        }
                                                    }
                                                }
                                                span { class: "block truncate text-[9px] font-mono uppercase tracking-wider text-[var(--text-dim)]",
                                                    if direct { "{fresh_label} \u{b7} direct" } else { "{fresh_label} \u{b7} relayed" }
                                                }
                                                if let Some(d) = entry.description.clone() {
                                                    span { class: "block truncate text-[11px] text-[var(--text-muted)]", "{d}" }
                                                }
                                            }
                                        }
                                        button {
                                            r#type: "button",
                                            class: "shrink-0 px-3 py-1 rounded-md border border-[var(--border)] text-[11px] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
                                            style: if fresh_ok { "" } else { "opacity: .6;" },
                                            onclick: move |_| on_enter.call(for_enter.clone()),
                                            "Enter"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn ws_to_http(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        url.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{JoinBy, Mode, initial_mode, join_by};

    const R: &str = "ws://rendezvous.example:7700";

    #[test]
    fn a_launch_opens_on_the_half_that_costs_nothing() {
        assert_eq!(initial_mode(), Mode::Join);
    }

    #[test]
    fn an_untouched_address_does_not_beat_a_code() {
        assert_eq!(join_by("", "purple-fox-42", R), JoinBy::Code);
    }

    #[test]
    fn a_typed_address_beats_a_code() {
        assert_eq!(
            join_by("ws://box.local:9000", "purple-fox-42", R),
            JoinBy::Url
        );
    }

    #[test]
    fn blank_space_is_not_an_address() {
        assert_eq!(join_by("   ", "purple-fox-42", R), JoinBy::Code);
    }

    #[test]
    fn a_code_without_a_directory_is_not_submittable() {
        assert_eq!(join_by("", "purple-fox-42", ""), JoinBy::Nothing);
    }

    #[test]
    fn an_empty_form_has_nothing_to_submit() {
        assert_eq!(join_by("", "", R), JoinBy::Nothing);
    }
}
