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

const INPUT: &str = "w-full bg-transparent border border-[var(--border)] rounded px-3 py-2 text-sm text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors";
const INPUT_SM: &str = "w-full bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors";
const LABEL: &str = "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]";

#[component]
pub fn ConnectView(
    identity: Identity,
    error: Option<String>,
    last_session: Option<SavedSession>,
    on_connect: EventHandler<SessionParams>,
    on_rename: EventHandler<String>,
    on_sign_out: EventHandler<()>,
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

    let mac_top_pad = if cfg!(target_os = "macos") {
        "pt-7"
    } else {
        "pt-0"
    };

    rsx! {
        div { class: "h-full w-full flex bg-[var(--bg)]",
            div {
                class: "dxf-drag-region hidden md:flex w-2/5 min-w-[340px] max-w-[520px] flex-col items-center justify-center px-10 bg-[var(--bg)]",
                onmousedown: move |_| crate::app::start_window_drag(),
                div {
                    class: "w-28 h-28 rounded-3xl flex items-center justify-center mb-8",
                    style: "background: linear-gradient(160deg, var(--panel2), var(--bg2)); \
                            border: 1px solid var(--edge); \
                            box-shadow: 0 0 60px -12px color-mix(in srgb, var(--accent) 45%, transparent);",
                    crate::app::DiscordiaLogo { class: "w-16 h-16" }
                }
                h1 { class: "dxf-display dxf-wordmark text-6xl font-extrabold tracking-tight",
                    "Discordia"
                }
                p { class: "text-[15px] text-[var(--text-muted)] mt-5 text-center max-w-[320px] leading-relaxed",
                    "Chat you actually own. Self-hosted, cryptographic identity, and a room you rearrange like furniture."
                }
                div { class: "flex flex-wrap items-center justify-center gap-2 mt-7",
                    span { class: "flex items-center gap-1.5 px-3 py-1.5 rounded-full border border-[var(--edge)] text-xs text-[var(--accent)]",
                        style: "background: var(--accent-soft);", "🔑 Nostr identity"
                    }
                    span { class: "flex items-center gap-1.5 px-3 py-1.5 rounded-full border border-[var(--edge)] text-xs",
                        style: "background: color-mix(in srgb, var(--up) 10%, transparent); color: var(--up);",
                        "⌂ Self-hosted"
                    }
                }
            }

            div { class: "flex-1 flex flex-col overflow-hidden min-w-0",
                div {
                    class: "dxf-drag-region h-8 shrink-0 {mac_top_pad}",
                    onmousedown: move |_| crate::app::start_window_drag(),
                }
                form {
                    class: "flex-1 overflow-auto px-8 py-8 flex flex-col items-stretch dxf-no-drag",
                    onsubmit: submit,
                    div { class: "w-full max-w-md mx-auto my-auto space-y-5 flex flex-col",

                IdentityCard { identity: identity.clone(), on_rename, on_sign_out }

                if let Some(saved) = last_session.clone() {
                    {
                        let identity_for_reconnect = identity.clone();
                        let on_connect_for_reconnect = on_connect;
                        rsx! {
                            button {
                                r#type: "button",
                                class: "panel-hover w-full flex items-center gap-2 border border-[var(--border)] hover:border-[var(--accent)] rounded p-2 text-xs text-left group",
                                onclick: move |_| {
                                    let params = SessionParams {
                                        mode: saved.mode.clone(),
                                        username: saved.username.clone(),
                                        identity: identity_for_reconnect.clone(),
                                    };
                                    on_connect_for_reconnect.call(params);
                                },
                                div { class: "flex-1 min-w-0",
                                    div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)]",
                                        "Last session"
                                    }
                                    div { class: "text-[var(--text)] truncate group-hover:text-[var(--accent)] transition-colors",
                                        "{session::label(&saved)}"
                                    }
                                }
                                span { class: "text-[10px] text-[var(--accent)] uppercase tracking-wider font-medium",
                                    "reconnect →"
                                }
                            }
                        }
                    }
                }

                div { class: "h-px bg-[var(--border)]" }

                div { class: "space-y-1.5",
                    RendezvousPicker {
                        selected: rendezvous_url(),
                        on_select: move |u: String| rendezvous_url.set(u),
                    }
                    div { class: "text-[10px] text-[var(--text-dim)] leading-relaxed",
                        "Codes are looked up here, the public list comes from here, and a server of your own is published and named here."
                    }
                }

                div { class: "flex gap-1 text-xs",
                    TabButton { active: mode() == Mode::Join, label: "Join a server", onclick: move |_| mode.set(Mode::Join) }
                    TabButton { active: mode() == Mode::Create, label: "Create a server", onclick: move |_| mode.set(Mode::Create) }
                }

                if let Some(err) = error {
                    div { class: "text-xs text-[var(--danger)] border border-[var(--border)] rounded px-3 py-2",
                        "{err}"
                    }
                }

                div { class: "fade-in flex-1",
                match mode() {
                    Mode::Join => rsx! {
                        div { class: "space-y-3",
                            div { class: "space-y-1",
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
                                picked_shortcode: code(),
                                rendezvous_url: rendezvous_url(),
                            }

                            details {
                                summary { class: "cursor-pointer text-[10px] uppercase tracking-wider text-[var(--text-muted)] hover:text-[var(--text)] transition-colors",
                                    "Other ways to connect"
                                }
                                div { class: "space-y-1 mt-2",
                                    label { class: LABEL, "Server address" }
                                    input {
                                        class: INPUT_SM,
                                        r#type: "text",
                                        placeholder: "ws://localhost:9000",
                                        value: "{server_url}",
                                        oninput: move |e| server_url.set(e.value()),
                                    }
                                    div { class: "text-[10px] text-[var(--text-dim)] leading-relaxed",
                                        "Connects straight to a gateway, ignoring the code and the directory. Leave it empty to use the code above."
                                    }
                                }
                            }
                        }
                    },
                    Mode::Create => rsx! {
                        div { class: "border border-[var(--border)] rounded p-3 text-xs space-y-3",
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
                                    title: "Friends here reach you directly, and Discordia asks your router (UPnP / NAT-PMP) to let in friends elsewhere. Your home IP becomes visible to anyone who joins that way.",
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

                button {
                    class: "dxf-cta w-full py-2.5 rounded-xl transition-all disabled:opacity-30 disabled:cursor-not-allowed text-sm",
                    r#type: "submit",
                    disabled,
                    {match mode() {
                        Mode::Join => "Connect  →",
                        Mode::Create => "Launch  →",
                    }}
                }
                    }
                }
            }
        }
    }
}

#[component]
fn RendezvousPicker(selected: String, on_select: EventHandler<String>) -> Element {
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
        div { class: "space-y-1.5",
            div { class: "flex items-center gap-2",
                span { class: "{LABEL} flex-1", "Rendezvous server" }
                button {
                    r#type: "button",
                    class: "text-[10px] uppercase tracking-wider text-[var(--accent)] hover:text-[var(--accent-strong)] transition-colors",
                    onclick: move |_| adding.set(!adding()),
                    if adding() { "cancel" } else { "+ add" }
                }
            }
            if adding() {
                div { class: "flex gap-1",
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
                        class: "px-2 rounded text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                        onclick: move |_| {
                            let v = draft().trim().to_string();
                            if !v.is_empty() { commit(v); }
                        },
                        "save"
                    }
                }
            }
            div { class: "flex items-center gap-1",
                select {
                    class: "flex-1 min-w-0 rounded border border-[var(--border)] px-2 py-1 font-mono text-[11px] focus:outline-none focus:border-[var(--accent)] transition-colors",
                    style: "color: var(--text); background: var(--panel-solid);",
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
                button {
                    r#type: "button",
                    class: "px-1.5 text-[var(--text-dim)] hover:text-[var(--danger)] text-xs transition-colors",
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
        "text-[var(--accent)] border-[var(--accent)]"
    } else {
        "text-[var(--text-muted)] border-transparent hover:text-[var(--text)]"
    };
    rsx! {
        button {
            r#type: "button",
            class: "flex-1 px-2 py-1.5 border-b font-medium transition-colors {cls}",
            onclick: move |_| onclick.call(()),
            "{label}"
        }
    }
}

#[component]
fn IdentityCard(
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
    let file_path = Identity::file_path_display();

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
                    title: "Wipe local identity",
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

#[component]
fn BrowseTab(
    rendezvous_url: String,
    on_pick: EventHandler<DiscoverEntry>,
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
            div { class: "flex items-center gap-2",
                span { class: "{LABEL} flex-1", "Public servers" }
                button {
                    r#type: "button",
                    class: "text-[10px] text-[var(--accent)] hover:text-[var(--accent-strong)] transition-colors",
                    onclick: move |_| refresh_tick.set(refresh_tick() + 1),
                    "↻ Refresh"
                }
            }

            div { class: "max-h-64 overflow-y-auto border border-[var(--border)] rounded",
                match &*entries.read_unchecked() {
                    None => rsx! {
                        div { class: "text-xs text-[var(--text-dim)] px-3 py-4 text-center", "Loading…" }
                    },
                    Some(Err(e)) => rsx! {
                        div { class: "text-xs px-3 py-4 space-y-1",
                            div { class: "text-[var(--danger)]",
                                "Couldn't reach the server directory."
                            }
                            div { class: "text-[var(--text-dim)]",
                                "A code from a friend still works, or create your own server. ({e})"
                            }
                        }
                    },
                    Some(Ok(list)) if list.is_empty() => rsx! {
                        div { class: "text-xs text-[var(--text-dim)] px-3 py-4 text-center",
                            "Nobody has listed a public server on this directory yet. A code from \
                             a friend works without one, or make your own from the Create tab."
                        }
                    },
                    Some(Ok(list)) => rsx! {
                        for entry in list.iter().cloned() {
                            {
                                let sc = entry.shortcode.clone();
                                let selected = picked_shortcode == sc;
                                let row_cls = if selected {
                                    "bg-[var(--accent-soft)] border-l-2 border-[var(--accent)]"
                                } else {
                                    "border-l-2 border-transparent hover:bg-white/[0.02]"
                                };
                                let entry_for_pick = entry.clone();
                                rsx! {
                                    button {
                                        key: "{sc}",
                                        r#type: "button",
                                        class: "w-full text-left px-3 py-2 {row_cls} transition-colors",
                                        onclick: move |_| on_pick.call(entry_for_pick.clone()),
                                        div { class: "flex items-baseline gap-2",
                                            span {
                                                class: "w-1.5 h-1.5 rounded-full shrink-0 self-center",
                                                style: if entry.idle_secs >= HOST_STALE_AFTER_SECS {
                                                    "background: var(--warn);"
                                                } else {
                                                    "background: var(--up);"
                                                },
                                                title: if entry.idle_secs >= HOST_STALE_AFTER_SECS {
                                                    "Not responding — this host may already be offline"
                                                } else {
                                                    "Online"
                                                },
                                            }
                                            span { class: "text-sm font-medium text-[var(--text)]",
                                                {entry.name.clone().unwrap_or_else(|| entry.shortcode.clone())}
                                            }
                                            span { class: "text-[10px] text-[var(--text-dim)]", "{entry.shortcode}" }
                                            if entry.idle_secs >= HOST_STALE_AFTER_SECS {
                                                span { class: "text-[10px] text-[var(--warn)]", "not responding" }
                                            }
                                        }
                                        if let Some(d) = entry.description.clone() {
                                            div { class: "text-xs text-[var(--text-muted)] mt-0.5", "{d}" }
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
