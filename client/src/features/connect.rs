use dioxus::prelude::*;

use crate::features::globe::{Globe, GlobePin, describe};
use crate::identity::{Identity, IdentitySource};
use crate::protocol::rendezvous::{DiscoverEntry, GeoPoint};
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

fn join_by(server_url: &str, code: &str, rendezvous: &[String]) -> JoinBy {
    if !server_url.trim().is_empty() {
        JoinBy::Url
    } else if !code.trim().is_empty() && !rendezvous.is_empty() {
        JoinBy::Code
    } else {
        JoinBy::Nothing
    }
}

const INPUT: &str = "w-full bg-[var(--panel2)] border border-[var(--edge-strong)] rounded-[11px] px-4 py-3.5 text-[14.5px] text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors";
const INPUT_SM: &str = "w-full bg-[var(--panel2)] border border-[var(--edge-strong)] rounded-[10px] px-3 py-2 text-[12.5px] text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors";
const LABEL: &str = "text-[11px] uppercase tracking-[0.09em] text-[var(--text-dim)]";

#[component]
pub(crate) fn ConnectionErrorPanel(message: String) -> Element {
    let mut copy_result = use_signal(|| None::<(String, bool)>);
    let result = copy_result()
        .filter(|(copied, _)| copied == &message)
        .map(|(_, ok)| ok);
    let to_copy = message.clone();
    let (explanation, details) = message.split_once("\n\n").unwrap_or((&message, ""));
    rsx! {
        div { role: "alert", class: "text-[12.5px] text-[var(--danger)] border border-[var(--border)] rounded-[11px] px-4 py-3",
            p { style: "overflow-wrap: anywhere; user-select: text;", "{explanation}" }
            if !details.is_empty() {
                details { class: "mt-2 text-[var(--text-muted)]",
                    summary { "Diagnostic details" }
                    div { style: "white-space: pre-wrap; overflow-wrap: anywhere; user-select: text; max-height: 20rem; overflow-y: auto;", "{details}" }
                }
            }
            button {
                r#type: "button",
                class: "mt-3 text-xs px-3 py-2 rounded-lg border border-[var(--border)] text-[var(--text)] hover:bg-[var(--panel2)]",
                onclick: move |_| {
                    let ok = crate::clipboard::copy_text(&to_copy);
                    copy_result.set(Some((to_copy.clone(), ok)));
                },
                if result == Some(true) { "Copied" } else { "Copy diagnostics" }
            }
            if result == Some(false) {
                p { class: "mt-2 text-xs", "Couldn't copy. Select the text above and copy it manually." }
            }
        }
    }
}

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
    let rendezvous = settings.read().rendezvous_servers.clone();
    let publish_target = settings.read().publish_rendezvous();
    let media = discordia_server::livekit_bundle::ports();
    let (media_ws, media_tcp, media_udp) = (media.ws, media.tcp, media.udp);

    let mut mode = use_signal(initial_mode);
    let mut server_url = use_signal(String::new);
    let mut allow_lan = use_signal(|| true);
    let mut manual_ip = use_signal(String::new);
    let mut publish_to_rendezvous = use_signal(|| true);
    let mut code = use_signal(String::new);
    // Where a listed host was picked, so its code is asked of that rendezvous.
    let mut picked = use_signal(|| None::<(String, String)>);
    let mut looking_up = use_signal(|| false);
    let mut lookup_error = use_signal(|| None::<String>);

    let mut publish_name = use_signal(String::new);
    let mut description = use_signal(String::new);
    let mut publish_public = use_signal(|| true);
    let guessed = use_hook(crate::tzgeo::guess);
    let mut location = use_signal(|| guessed);

    let identity_for_rows = identity.clone();
    let identity_for_submit = identity.clone();
    let rendezvous_for_submit = rendezvous.clone();
    let publish_for_submit = publish_target.clone();
    let submit = move |_| {
        lookup_error.set(None);
        let params = match mode() {
            Mode::Join => match join_by(&server_url(), &code(), &rendezvous_for_submit) {
                JoinBy::Url => SessionParams {
                    mode: SessionMode::Remote {
                        server_url: server_url().trim().to_string(),
                    },
                    identity: identity_for_submit.clone(),
                },
                JoinBy::Code => {
                    let typed = code().trim().to_string();
                    let known = picked()
                        .filter(|(_, c)| *c == typed)
                        .map(|(r, _)| r)
                        .or_else(|| match rendezvous_for_submit.as_slice() {
                            [only] => Some(only.clone()),
                            _ => None,
                        });
                    match known {
                        Some(r) => by_code(r, typed, identity_for_submit.clone()),
                        None => {
                            let all = rendezvous_for_submit.clone();
                            let identity = identity_for_submit.clone();
                            looking_up.set(true);
                            spawn(async move {
                                let found = crate::net::find_code(&all, &typed).await;
                                looking_up.set(false);
                                match found {
                                    Some(r) => on_connect.call(by_code(r, typed, identity)),
                                    None => lookup_error.set(Some(format!(
                                        "No rendezvous in your list knows \u{201c}{typed}\u{201d}. Check the code, or add the rendezvous your friend publishes on."
                                    ))),
                                }
                            });
                            return;
                        }
                    }
                }
                JoinBy::Nothing => return,
            },
            Mode::Create => {
                let r_url = publish_to_rendezvous().then(|| publish_for_submit.clone());
                let pn = publish_name().trim().to_string();
                let desc = description().trim().to_string();
                SessionParams {
                    mode: SessionMode::SelfHost {
                        allow_lan: allow_lan(),
                        manual_ip: if allow_lan() {
                            manual_ip().trim().parse().ok()
                        } else {
                            None
                        },
                        rendezvous_url: r_url,
                        publish_name: if pn.is_empty() { None } else { Some(pn) },
                        description: if desc.is_empty() { None } else { Some(desc) },
                        publish_public: publish_to_rendezvous() && publish_public(),
                        location: location()
                            .filter(|_| publish_to_rendezvous() && publish_public()),
                    },
                    identity: identity_for_submit.clone(),
                }
            }
        };
        on_connect.call(params);
    };

    let bad_ip = allow_lan()
        && !manual_ip().trim().is_empty()
        && manual_ip()
            .trim()
            .parse::<std::net::IpAddr>()
            .ok()
            .is_none_or(|ip| !ip.is_ipv4() || !crate::host::public_address(ip));
    let publish_code = crate::protocol::rendezvous::join_code(&publish_name());
    let bad_name =
        publish_to_rendezvous() && !publish_name().trim().is_empty() && publish_code.is_none();
    let disabled = match mode() {
        Mode::Create => bad_ip || bad_name,
        Mode::Join => {
            looking_up() || join_by(&server_url(), &code(), &rendezvous) == JoinBy::Nothing
        }
    };
    // Going grey says "not yet" without saying why; this names both ways in.
    let hint = match last_session.as_ref() {
        Some(s) => format!("Enter a code above, or reconnect to {}.", session::label(s)),
        None => "Enter a code above to connect.".to_string(),
    };

    rsx! {
        div { class: "flex-1 min-h-0 w-full flex flex-col min-w-0",
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
                    RendezvousList {}
                    div { class: "text-xs text-[var(--text-dim)] leading-relaxed text-pretty",
                        "The public list gathers every one of these, and a code is looked up in all of them. A server of your own is published on one."
                    }
                }

                div { class: "flex gap-1 p-1 rounded-[11px] bg-[var(--panel2)] border border-[var(--edge)]",
                    TabButton { active: mode() == Mode::Join, label: "Join a server", onclick: move |_| mode.set(Mode::Join) }
                    TabButton { active: mode() == Mode::Create, label: "Create a server", onclick: move |_| mode.set(Mode::Create) }
                }

                if let Some(err) = lookup_error().or(error) {
                    ConnectionErrorPanel { message: err }
                }

                for (panel, row_identity) in std::iter::once((mode(), identity_for_rows.clone())) {
                div { key: "{panel:?}", class: "fade-in",
                match panel {
                    Mode::Join => rsx! {
                        div { class: "space-y-4",
                            div { class: "space-y-2",
                                label { class: LABEL, "Code" }
                                input {
                                    class: INPUT,
                                    r#type: "text",
                                    placeholder: "purple-fox-42 or a server name",
                                    value: "{code}",
                                    oninput: move |e| code.set(e.value()),
                                }
                            }

                            BrowseTab {
                                on_pick: move |host: Listed| {
                                    code.set(host.entry.shortcode.clone());
                                    picked.set(Some((host.rendezvous, host.entry.shortcode)));
                                },
                                on_enter: move |host: Listed| {
                                    on_connect.call(by_code(host.rendezvous, host.entry.shortcode, row_identity.clone()));
                                },
                                picked_shortcode: code(),
                                picked_rendezvous: picked().filter(|(_, c)| *c == code().trim()).map(|(r, _)| r),
                                rendezvous: rendezvous.clone(),
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
                                "Your machine runs the server and keeps its history. Voice runs here too, unless the rendezvous it is published on supplies its own."
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
                                    title: "Offers IPv4 and IPv6 addresses and asks your router (UPnP / NAT-PMP / PCP) to let friends in. Your home IP becomes visible to anyone who joins directly.",
                                    "?"
                                }
                            }
                            if allow_lan() {
                                details { class: "space-y-2 text-[12px] text-[var(--text-dim)]",
                                    summary { class: "cursor-pointer", "I already forwarded the ports" }
                                    label { class: LABEL, "Public IPv4 address (optional)" }
                                    input {
                                        class: INPUT_SM,
                                        placeholder: "Your router's public IPv4 address",
                                        value: "{manual_ip}",
                                        oninput: move |e| manual_ip.set(e.value()),
                                    }
                                    if bad_ip {
                                        div { class: "text-[var(--warn)]",
                                            "Enter a public IPv4 address, or leave this empty to try automatic detection."
                                        }
                                    }
                                    div { class: "text-[11px] leading-relaxed",
                                        "Forward UDP 9001 for joining, TCP {media_ws} for voice connections, TCP {media_tcp} and UDP {media_udp} for media to this PC. IPv6 is tried automatically."
                                    }
                                }
                            }
                            if publish_to_rendezvous() {
                                div { class: "pl-3 border-l border-[var(--border)] space-y-2",
                                    div { class: "space-y-1",
                                        label { class: LABEL, "Publish on" }
                                        div { class: "relative",
                                            select {
                                                // The native arrow is the one part of a select that no
                                                // stylesheet reaches, so it is replaced rather than styled.
                                                class: "w-full appearance-none rounded-[10px] border border-[var(--edge-strong)] pl-3.5 pr-9 py-2 font-mono text-[12.5px] focus:outline-none focus:border-[var(--accent)] transition-colors",
                                                style: "color: var(--text); background: var(--panel2);",
                                                onchange: move |e| {
                                                    let mut next = settings.read().clone();
                                                    next.publish_rendezvous = Some(e.value());
                                                    settings.set(next.clone());
                                                    crate::settings::save(&next);
                                                },
                                                for url in rendezvous.iter().cloned() {
                                                    option {
                                                        key: "{url}",
                                                        value: "{url}",
                                                        selected: url == publish_target,
                                                        style: "color: var(--text); background: var(--panel-solid);",
                                                        "{url}"
                                                    }
                                                }
                                            }
                                            span { class: "absolute right-3.5 top-1/2 -translate-y-1/2 text-[10px] text-[var(--text-dim)] pointer-events-none",
                                                "\u{25bc}"
                                            }
                                        }
                                    }
                                    div { class: "space-y-1",
                                        label { class: LABEL, "Server name" }
                                        input {
                                            class: INPUT_SM,
                                            r#type: "text",
                                            placeholder: "Friday Game Night",
                                            maxlength: crate::protocol::rendezvous::MAX_NAME_CHARS as i64,
                                            value: "{publish_name}",
                                            oninput: move |e| publish_name.set(e.value()),
                                        }
                                        if bad_name {
                                            div { class: "text-[10px] text-[var(--warn)]",
                                                "The join code is made from the letters a–z and digits in the name, and this one has none."
                                            }
                                        } else if let Some(c) = publish_code.clone() {
                                            div { class: "text-[10px] text-[var(--text-dim)]",
                                                "Friends join with the code "
                                                span { class: "font-mono text-[var(--text-muted)]", "{c}" }
                                                ", reserved to your key."
                                            }
                                        } else {
                                            div { class: "text-[10px] text-[var(--text-dim)]",
                                                "Shown in the server list and reserved to your key. Leave it empty for a random code."
                                            }
                                        }
                                    }
                                    ReservedNames {
                                        rendezvous_url: publish_target.clone(),
                                        identity: row_identity.clone(),
                                        on_pick: move |code: String| publish_name.set(code),
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
                                    if publish_public() {
                                        div { class: "space-y-1",
                                            label { class: LABEL, "Put it on the globe (optional)" }
                                            div { class: "rounded-[10px] bg-[var(--panel2)] border border-[var(--edge-strong)] overflow-hidden",
                                                Globe {
                                                    pins: Vec::<GlobePin>::new(),
                                                    selected: None::<String>,
                                                    pick: true,
                                                    place: location(),
                                                    height: 190,
                                                    on_pick: move |_| {},
                                                    on_place: move |p: GeoPoint| location.set(Some(p)),
                                                }
                                            }
                                            div { class: "flex items-center gap-2 text-[10px] text-[var(--text-dim)]",
                                                match location() {
                                                    Some(p) => rsx! {
                                                        span { class: "flex-1",
                                                            if Some(p) == guessed {
                                                                "Guessed from this machine's timezone: {describe(p)}. Click the globe to move it. Everyone browsing sees this spot."
                                                            } else {
                                                                "Pinned at {describe(p)}. Everyone browsing sees this spot."
                                                            }
                                                        }
                                                        button {
                                                            r#type: "button",
                                                            class: "shrink-0 text-[var(--accent)] hover:text-[var(--accent-strong)] transition-colors",
                                                            onclick: move |_| location.set(None),
                                                            "Remove"
                                                        }
                                                    },
                                                    None => rsx! {
                                                        span { class: "flex-1", "Click roughly where it lives. Kept to about a town, and shown to anyone browsing." }
                                                    },
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                }
                }
                }

                div { class: "mt-auto pt-2",
                    button {
                        class: "dxf-cta w-full py-3.5 rounded-xl transition-all disabled:cursor-not-allowed text-[15px]",
                        r#type: "submit",
                        disabled,
                        {match mode() {
                            Mode::Join if looking_up() => "Looking up the code\u{2026}",
                            Mode::Join => "Connect  →",
                            Mode::Create => "Launch  →",
                        }}
                    }
                    if disabled && !looking_up() && mode() == Mode::Join {
                        div { class: "mt-2 text-center text-xs text-[var(--text-dim)]", "{hint}" }
                    }
                }
                    }
                }
            }
        }
    }
}

fn by_code(rendezvous_url: String, code: String, identity: Identity) -> SessionParams {
    SessionParams {
        mode: SessionMode::ByCode {
            rendezvous_url,
            code,
        },
        identity,
    }
}

#[component]
fn RendezvousList() -> Element {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let mut adding = use_signal(|| false);
    let mut host = use_signal(String::new);
    let mut port = use_signal(|| crate::settings::RENDEZVOUS_PORT.to_string());
    let mut secure = use_signal(|| false);
    let servers = settings.read().rendezvous_servers.clone();
    let removable = servers.len() > 1;
    let candidate = compose_rendezvous(&host(), secure(), &port());
    let known = candidate.as_ref().is_some_and(|url| servers.contains(url));
    let addable = candidate.clone().filter(|_| !known);

    let mut store = move |next: crate::settings::ClientSettings| {
        settings.set(next.clone());
        crate::settings::save(&next);
    };
    let mut commit = move || {
        let Some(url) = compose_rendezvous(&host(), secure(), &port()) else {
            return;
        };
        let mut next = settings.read().clone();
        if next.add_rendezvous(&url).is_some() {
            store(next);
            host.set(String::new());
            port.set(crate::settings::RENDEZVOUS_PORT.to_string());
            secure.set(false);
            adding.set(false);
        }
    };

    rsx! {
        div { class: "space-y-2",
            div { class: "flex items-baseline gap-2",
                span { class: "{LABEL} flex-1", "Rendezvous servers" }
                button {
                    r#type: "button",
                    class: "text-[12.5px] font-medium text-[var(--accent)] hover:text-[var(--accent-strong)] transition-colors",
                    onclick: move |_| adding.set(!adding()),
                    if adding() { "Cancel" } else { "+ Add" }
                }
            }
            div { class: "space-y-1.5",
                for (url, forget) in servers.iter().map(|u| (u.clone(), u.clone())) {
                    div {
                        key: "{url}",
                        class: "flex items-center gap-2 rounded-[10px] border border-[var(--edge-strong)] bg-[var(--panel2)] pl-3.5 pr-2 py-1",
                        span { class: "flex-1 min-w-0 truncate font-mono text-[12.5px] text-[var(--text)]", title: "{url}", "{url}" }
                        // The last one stays: without a rendezvous there is no list and no code.
                        if removable {
                            button {
                                r#type: "button",
                                class: "w-7 h-7 shrink-0 rounded-md flex items-center justify-center text-xs text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                                title: "Forget this rendezvous",
                                onclick: move |_| {
                                    let mut next = settings.read().clone();
                                    next.remove_rendezvous(&forget);
                                    store(next);
                                },
                                "\u{2715}"
                            }
                        }
                    }
                }
            }
            if adding() {
                div { class: "space-y-2 px-4 py-3.5 rounded-[11px] bg-[var(--panel)] border border-[var(--edge)]",
                    div { class: "flex gap-1.5",
                        div { class: "flex-1 min-w-0 space-y-1",
                            label { class: LABEL, "Address" }
                            input {
                                class: "{INPUT_SM} font-mono",
                                r#type: "text",
                                placeholder: "rendezvous.example.com",
                                value: "{host}",
                                autofocus: true,
                                oninput: move |e| host.set(e.value()),
                                // A pasted full address fills the other fields, once typing is done.
                                onchange: move |e| {
                                    if let Some(parts) = crate::settings::split_rendezvous(&e.value()) {
                                        if let Some(s) = parts.secure { secure.set(s); }
                                        if let Some(p) = parts.port { port.set(p.to_string()); }
                                        host.set(parts.host);
                                    }
                                },
                                onkeydown: move |e| {
                                    if e.key() == Key::Enter {
                                        e.prevent_default();
                                        commit();
                                    } else if e.key() == Key::Escape {
                                        adding.set(false);
                                    }
                                },
                            }
                        }
                        div { class: "shrink-0 space-y-1", style: "width: 5.5rem;",
                            label { class: LABEL, "Port" }
                            input {
                                class: "{INPUT_SM} font-mono",
                                r#type: "text",
                                inputmode: "numeric",
                                maxlength: 5,
                                placeholder: "{crate::settings::default_rendezvous_port(secure())}",
                                value: "{port}",
                                oninput: move |e| port.set(e.value().chars().filter(char::is_ascii_digit).collect()),
                                onkeydown: move |e| {
                                    if e.key() == Key::Enter {
                                        e.prevent_default();
                                        commit();
                                    }
                                },
                            }
                        }
                    }
                    div { class: "flex items-center gap-2",
                        label {
                            class: "flex-1 flex items-center gap-2 cursor-pointer text-[12px] text-[var(--text)]",
                            title: "Only for a rendezvous behind a TLS proxy. Most are not, so leave this off unless its owner says otherwise.",
                            input {
                                r#type: "checkbox",
                                checked: secure(),
                                oninput: move |e| {
                                    let on = e.value() == "true";
                                    let was = crate::settings::default_rendezvous_port(secure()).to_string();
                                    if port().is_empty() || port() == was {
                                        port.set(crate::settings::default_rendezvous_port(on).to_string());
                                    }
                                    secure.set(on);
                                },
                            }
                            "Secure connection (wss://)"
                        }
                        button {
                            r#type: "button",
                            class: "px-3 py-1.5 rounded-[10px] text-[12.5px] font-medium text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors disabled:opacity-50 disabled:cursor-not-allowed",
                            disabled: addable.is_none(),
                            onclick: move |_| commit(),
                            "Add"
                        }
                    }
                    div { class: "text-[11px] text-[var(--text-dim)] font-mono truncate",
                        match (candidate, known) {
                            (Some(url), true) => rsx! { "{url} is already in the list" },
                            (Some(url), false) => rsx! { "Adds {url}" },
                            (None, _) if host().trim().is_empty() => rsx! { "Just the name or IP is enough; the port is filled in." },
                            (None, _) => rsx! { "That isn't an address" },
                        }
                    }
                }
            }
        }
    }
}

/// Explicit parts of what was typed in the address box beat the other fields,
/// so a pasted `wss://…` is right even before it is split out.
fn compose_rendezvous(host: &str, secure: bool, port: &str) -> Option<String> {
    let parts = crate::settings::split_rendezvous(host)?;
    let secure = parts.secure.unwrap_or(secure);
    let port = match parts.port {
        Some(p) => p,
        None if port.trim().is_empty() => crate::settings::default_rendezvous_port(secure),
        None => port.trim().parse().ok().filter(|p| *p != 0)?,
    };
    Some(crate::settings::rendezvous_url(secure, &parts.host, port))
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

/// Where saved emoji pictures go so a DM can link them. On the account card,
/// where DMs live, and in the profile editor beside the emoji it serves.
#[component]
pub(crate) fn BlobServerField() -> Element {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let mut adding = use_signal(|| false);
    let mut draft = use_signal(String::new);
    let mut note = use_signal(|| None::<String>);
    let current = settings.read().emoji_blob_server.clone();
    let choices = settings.read().blob_server_choices();
    let removable = current
        .as_ref()
        .filter(|url| settings.read().blob_servers.contains(url))
        .cloned();
    let mut store = move |next: crate::settings::ClientSettings| {
        settings.set(next.clone());
        crate::settings::save(&next);
    };
    let mut add = move || {
        let Some(url) = crate::nostr::blossom::normalize_server(&draft()) else {
            note.set(Some("The address must start with https://.".into()));
            return;
        };
        let mut next = settings.read().clone();
        next.add_blob_server(url);
        store(next);
        draft.set(String::new());
        adding.set(false);
        note.set(None);
    };
    let summary = match &current {
        Some(url) => format!("Emoji in DMs — {}", blob_host(url)),
        None => "Emoji in DMs — off".to_string(),
    };
    rsx! {
        details { class: "text-[10px] text-[var(--text-dim)]",
            summary { class: "cursor-pointer hover:text-[var(--text-muted)] transition-colors", "{summary}" }
            div { class: "mt-1 text-[var(--text-muted)]",
                "Your saved emoji's pictures upload here so a DM can link them. Off: only small pictures fit inside the message; large ones go as text."
            }
            div { class: "flex items-center gap-1 mt-1.5",
                div { class: "flex-1 min-w-0 relative",
                    select {
                        class: "w-full appearance-none rounded border border-[var(--border)] pl-2 py-1 font-mono text-[11px] focus:outline-none focus:border-[var(--accent)] transition-colors",
                        style: "color: var(--text); background: var(--panel2); padding-right: 1.5rem;",
                        onchange: move |e| {
                            let value = e.value();
                            let mut next = settings.read().clone();
                            next.emoji_blob_server = (value != BLOB_OFF).then_some(value);
                            store(next);
                            note.set(None);
                        },
                        for url in choices {
                            option {
                                key: "{url}",
                                value: "{url}",
                                selected: current.as_deref() == Some(url.as_str()),
                                style: "color: var(--text); background: var(--panel-solid);",
                                "{blob_host(&url)}"
                            }
                        }
                        option {
                            value: BLOB_OFF,
                            selected: current.is_none(),
                            style: "color: var(--text); background: var(--panel-solid);",
                            "Off — inside the message"
                        }
                    }
                    span {
                        class: "absolute top-1/2 -translate-y-1/2 text-[8px] text-[var(--text-dim)] pointer-events-none",
                        style: "right: 0.5rem;",
                        "▼"
                    }
                }
                if let Some(url) = removable {
                    button {
                        r#type: "button",
                        class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] border border-[var(--border)] rounded px-2 py-1 hover:text-[var(--danger)] transition-colors",
                        title: "Remove this server from the list",
                        onclick: move |_| {
                            let mut next = settings.read().clone();
                            next.remove_blob_server(&url);
                            store(next);
                        },
                        "Remove"
                    }
                }
                button {
                    r#type: "button",
                    class: "text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] rounded px-2 py-1 hover:border-[var(--accent)] transition-colors",
                    onclick: move |_| { adding.toggle(); note.set(None); },
                    if adding() { "Cancel" } else { "+ Add" }
                }
            }
            if adding() {
                div { class: "flex gap-1 mt-1",
                    input {
                        class: "flex-1 min-w-0 bg-transparent border border-[var(--border)] rounded px-2 py-1 text-[11px] text-[var(--text)] font-mono focus:outline-none focus:border-[var(--accent)]",
                        placeholder: "https://blossom.example",
                        value: "{draft}",
                        autofocus: true,
                        oninput: move |e| draft.set(e.value()),
                        onkeydown: move |e| {
                            if e.key() == Key::Enter {
                                add();
                            } else if e.key() == Key::Escape {
                                adding.set(false);
                            }
                        },
                    }
                    button {
                        r#type: "button",
                        class: "text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] rounded px-2 hover:border-[var(--accent)] transition-colors",
                        onclick: move |_| add(),
                        "Save"
                    }
                }
            }
            if let Some(n) = note() {
                div { class: "mt-1 text-[var(--text-muted)]", "{n}" }
            }
        }
    }
}

/// Every name a host has launched under stays reserved until a day after its
/// last session, and a key holds only a few: this is where one is let go early.
#[component]
fn ReservedNames(
    rendezvous_url: ReadSignal<String>,
    identity: Identity,
    on_pick: EventHandler<String>,
) -> Element {
    let mut refresh = use_signal(|| 0u32);
    let mut releasing = use_signal(std::collections::HashSet::<String>::new);
    let mut note = use_signal(|| None::<String>);
    let lister = identity.clone();
    let names = use_resource(move || {
        let _ = refresh();
        let url = rendezvous_url().trim().to_string();
        let identity = lister.clone();
        async move {
            if url.is_empty() {
                return Ok(Vec::new());
            }
            crate::rendezvous::list_names(&url, &identity).await
        }
    });
    let listed = match &*names.read_unchecked() {
        Some(Ok(list)) => list.clone(),
        Some(Err(e)) => {
            return rsx! {
                div { class: "text-[10px] text-[var(--text-dim)]", "Couldn't list your names here: {e}" }
            };
        }
        None => return rsx! {},
    };
    if listed.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "space-y-1",
            label { class: LABEL, "Your names here" }
            for name in listed {
                {
                    let pending = releasing.read().contains(&name.code);
                    let code = name.code.clone();
                    let freed = name.code.clone();
                    let identity = identity.clone();
                    let status = match name.expires_in_secs {
                        None => "live now".to_string(),
                        Some(s) if s >= 3600 => format!("free in {}h", s / 3600),
                        Some(s) => format!("free in {}m", (s / 60).max(1)),
                    };
                    rsx! {
                        div {
                            key: "{name.code}",
                            class: "flex items-center gap-2 text-[11px]",
                            style: if pending { "opacity: .5;" } else { "" },
                            button {
                                r#type: "button",
                                class: "font-mono text-[var(--text)] hover:text-[var(--accent)] truncate transition-colors",
                                title: "Launch under this name again",
                                onclick: move |_| on_pick.call(code.clone()),
                                "{name.code}"
                            }
                            span { class: "flex-1 text-[var(--text-dim)] whitespace-nowrap", "{status}" }
                            if name.expires_in_secs.is_some() {
                                button {
                                    r#type: "button",
                                    class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                                    title: "Free this name now, for anyone to take",
                                    disabled: pending,
                                    onclick: move |_| {
                                        let code = freed.clone();
                                        let identity = identity.clone();
                                        releasing.write().insert(code.clone());
                                        note.set(None);
                                        spawn(async move {
                                            let url = rendezvous_url().trim().to_string();
                                            let result = crate::rendezvous::release_name(&url, &identity, &code).await;
                                            // Kept dimmed on success until the refreshed list drops it.
                                            if let Err(e) = result {
                                                note.set(Some(format!("Couldn't release {code}: {e}")));
                                                releasing.write().remove(&code);
                                            }
                                            refresh += 1;
                                        });
                                    },
                                    if pending { "Releasing…" } else { "Release" }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(n) = note() {
                div { class: "text-[10px] text-[var(--danger)]", "{n}" }
            }
        }
    }
}

const BLOB_OFF: &str = "off";

fn blob_host(url: &str) -> &str {
    url.strip_prefix("https://").unwrap_or(url)
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
    let phrase = match &identity.source {
        IdentitySource::Phrase(p) => Some(p.clone()),
        IdentitySource::Nsec(_) => None,
    };

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
                    div { class: "h-2 flex-1 rounded-full", style: "background-color: {c};" }
                }
            }
            div { class: "text-[11px] text-[var(--text-dim)]",
                "This color signature is derived from your public key. Nobody else has it."
            }
            RecoveryPhrase { phrase }
            BlobServerField {}
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reveal {
    Hidden,
    Confirming,
    Shown,
}

const REVEAL_FOR: std::time::Duration = std::time::Duration::from_secs(60);

/// Two clicks, not one, and it closes itself: the phrase is the whole account,
/// and the card it sits in stays open as long as the window does.
#[component]
fn RecoveryPhrase(phrase: Option<String>) -> Element {
    let mut reveal = use_signal(|| Reveal::Hidden);
    let mut generation = use_signal(|| 0u32);

    let Some(phrase) = phrase else {
        return rsx! {
            div { class: "text-[10px] text-[var(--text-dim)]",
                "Imported from a private key, so there is no recovery phrase. Keep the key string itself."
            }
        };
    };
    let mut show = move || {
        let current = generation() + 1;
        generation.set(current);
        reveal.set(Reveal::Shown);
        spawn(async move {
            tokio::time::sleep(REVEAL_FOR).await;
            if generation() == current {
                reveal.set(Reveal::Hidden);
            }
        });
    };

    rsx! {
        div { class: "space-y-1.5",
            div { class: "flex items-center gap-2",
                span { class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)]",
                    "Recovery phrase"
                }
                span { class: "flex-1" }
                match reveal() {
                    Reveal::Hidden => rsx! {
                        button {
                            r#type: "button",
                            class: "text-[10px] text-[var(--accent)] hover:text-[var(--accent-strong)] uppercase tracking-wider",
                            onclick: move |_| reveal.set(Reveal::Confirming),
                            "Reveal"
                        }
                    },
                    Reveal::Confirming => rsx! {
                        button {
                            r#type: "button",
                            class: "text-[10px] text-[var(--text-muted)] hover:text-[var(--text)] uppercase tracking-wider",
                            onclick: move |_| reveal.set(Reveal::Hidden),
                            "Cancel"
                        }
                    },
                    Reveal::Shown => rsx! {
                        button {
                            r#type: "button",
                            class: "text-[10px] text-[var(--accent)] hover:text-[var(--accent-strong)] uppercase tracking-wider",
                            onclick: move |_| {
                                generation.set(generation() + 1);
                                reveal.set(Reveal::Hidden);
                            },
                            "Hide"
                        }
                    },
                }
            }
            match reveal() {
                Reveal::Hidden => rsx! {
                    div { class: "text-[10px] text-[var(--text-dim)]",
                        "Twelve words that restore this identity on any device."
                    }
                },
                Reveal::Confirming => rsx! {
                    p { class: "text-[10px] text-[var(--warn)]",
                        "Anyone who sees these words owns this identity, and any wallet that shares the phrase. Make sure nobody is looking or recording."
                    }
                    button {
                        r#type: "button",
                        class: "w-full text-[10px] text-[var(--warn)] hover:text-[var(--text)] uppercase tracking-wider border border-[var(--warn)] rounded-md px-2.5 py-1 transition-colors",
                        onclick: move |_| show(),
                        "Show the phrase"
                    }
                },
                Reveal::Shown => rsx! {
                    div { class: "text-sm font-mono text-[var(--text)] border border-[var(--edge)] rounded-xl p-3 leading-relaxed select-all break-words",
                        style: "background: var(--bg2);",
                        "{phrase}"
                    }
                    div { class: "text-[10px] text-[var(--text-dim)]",
                        "Hides by itself after a minute."
                    }
                },
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

/// A host as one rendezvous lists it. Codes are unique only within one, so the
/// pair is the identity of a row and a pin.
#[derive(Debug, Clone, PartialEq)]
pub struct Listed {
    rendezvous: String,
    entry: DiscoverEntry,
}

impl Listed {
    fn key(&self) -> String {
        listed_key(&self.rendezvous, &self.entry.shortcode)
    }
}

fn listed_key(rendezvous: &str, code: &str) -> String {
    format!("{rendezvous} {code}")
}

fn rendezvous_label(url: &str) -> &str {
    url.split_once("://").map_or(url, |(_, rest)| rest)
}

async fn discover(rendezvous: &str) -> Result<Vec<DiscoverEntry>, String> {
    let url = format!("{}/discover", ws_to_http(rendezvous));
    reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| format!("fetch: {e}"))?
        .json::<Vec<DiscoverEntry>>()
        .await
        .map_err(|e| format!("decode: {e}"))
}

/// Every rendezvous's hosts in list order, and the ones that did not answer.
fn merge(
    answers: Vec<(String, Result<Vec<DiscoverEntry>, String>)>,
) -> (Vec<Listed>, Vec<(String, String)>) {
    let mut hosts = Vec::new();
    let mut failed = Vec::new();
    for (rendezvous, answer) in answers {
        match answer {
            Ok(entries) => hosts.extend(entries.into_iter().map(|entry| Listed {
                rendezvous: rendezvous.clone(),
                entry,
            })),
            Err(e) => failed.push((rendezvous, e)),
        }
    }
    (hosts, failed)
}

#[component]
fn BrowseTab(
    rendezvous: ReadSignal<Vec<String>>,
    on_pick: EventHandler<Listed>,
    on_enter: EventHandler<Listed>,
    picked_shortcode: String,
    picked_rendezvous: Option<String>,
) -> Element {
    let mut refresh_tick = use_signal(|| 0u32);
    let answers = use_resource(move || {
        let _ = refresh_tick();
        let all = rendezvous();
        async move {
            let asks = all
                .iter()
                .map(|r| async move { (r.clone(), discover(r).await) });
            merge(futures_util::future::join_all(asks).await)
        }
    });

    let (list, failed) = match &*answers.read_unchecked() {
        Some((l, f)) => (l.clone(), f.clone()),
        None => (Vec::new(), Vec::new()),
    };
    let selected = list
        .iter()
        .find(|h| {
            h.entry.shortcode == picked_shortcode
                && picked_rendezvous
                    .as_ref()
                    .is_none_or(|r| *r == h.rendezvous)
        })
        .map(Listed::key);
    let pins: Vec<GlobePin> = list
        .iter()
        .filter_map(|h| {
            let p = h.entry.location?;
            let name = h.entry.name.as_ref().unwrap_or(&h.entry.shortcode);
            Some(GlobePin {
                code: h.key(),
                label: format!("{name} \u{b7} {}", rendezvous_label(&h.rendezvous)),
                lat: p.lat,
                lon: p.lon,
                fresh: h.entry.idle_secs < HOST_STALE_AFTER_SECS,
            })
        })
        .collect();
    let pin_list = list.clone();

    let status = match &*answers.read_unchecked() {
        None => Some(rsx! {
            div { class: "text-[12.5px] text-[var(--text-dim)] px-6 py-5 text-center border border-[var(--edge-strong)] rounded-xl", "Loading\u{2026}" }
        }),
        Some((l, f)) if l.is_empty() && !f.is_empty() && f.len() == rendezvous().len() => {
            Some(rsx! {
                div { class: "text-[12.5px] px-6 py-5 space-y-1.5 border border-[var(--edge-strong)] rounded-xl",
                    div { class: "font-semibold text-[var(--danger)]", "Couldn't reach any server directory." }
                    div { class: "text-[var(--text-dim)] leading-relaxed text-pretty",
                        "A code from a friend still works, or create your own server."
                    }
                }
            })
        }
        Some((l, _)) if l.is_empty() => Some(rsx! {
            div { class: "px-6 py-5 text-center bg-[var(--panel)] border border-dashed border-[var(--edge-strong)] rounded-xl",
                div { class: "text-sm font-semibold text-[var(--text-muted)]", "Nothing listed yet" }
                div { class: "mt-1.5 mx-auto max-w-[380px] text-[12.5px] text-[var(--text-dim)] leading-relaxed text-pretty",
                    "A code from a friend works without one \u{2014} or make your own from the Create tab."
                }
            }
        }),
        Some(_) => None,
    };

    rsx! {
        div { class: "space-y-2",
            div { class: "flex items-center gap-2",
                span { class: "{LABEL} flex-1", "Public servers" }
                button {
                    r#type: "button",
                    class: "text-[12.5px] font-medium text-[var(--accent)] hover:text-[var(--accent-strong)] transition-colors",
                    onclick: move |_| refresh_tick.set(refresh_tick() + 1),
                    "\u{21bb} Refresh"
                }
            }
            if let Some(st) = status {
                {st}
            } else {
                div {
                    class: "overflow-y-auto space-y-1.5 pr-0.5",
                    style: "max-height: {HOST_LIST_MAX_HEIGHT};",
                    for host in list.iter().cloned() {
                        HostRow {
                            key: "{host.key()}",
                            selected: selected.as_deref() == Some(host.key().as_str()),
                            host,
                            on_pick,
                            on_enter,
                        }
                    }
                }
            }
            for (url, e) in failed {
                div {
                    key: "{url}",
                    class: "text-[11px] text-[var(--text-dim)] truncate",
                    title: "{e}",
                    span { class: "text-[var(--danger)]", "Unreachable: " }
                    span { class: "font-mono", "{rendezvous_label(&url)}" }
                }
            }
            div { class: "rounded-xl bg-[var(--panel)] border border-[var(--edge-strong)] overflow-hidden",
                Globe {
                    pins,
                    selected,
                    pick: false,
                    place: None::<GeoPoint>,
                    height: 260,
                    on_pick: move |key: String| {
                        if let Some(h) = pin_list.iter().find(|h| h.key() == key) {
                            on_pick.call(h.clone());
                            scroll_to_host(&key);
                        }
                    },
                    on_place: move |_| {},
                }
            }
            div { class: "text-[11px] text-[var(--text-dim)] text-center",
                "Drag to spin. Click a pin to pick that server."
            }
        }
    }
}

/// Six rows, each `HOST_ROW_HEIGHT`, with the list's 0.375rem gaps between.
const HOST_LIST_MAX_HEIGHT: &str = "calc(6 * 4.25rem + 5 * 0.375rem)";
const HOST_ROW_HEIGHT: &str = "4.25rem";

/// A pin can belong to a row scrolled out of the list's six.
fn scroll_to_host(key: &str) {
    let id = serde_json::to_string(&host_row_id(key)).unwrap_or_default();
    document::eval(&format!(
        "document.getElementById({id})?.scrollIntoView({{block: 'nearest'}});"
    ));
}

fn host_row_id(key: &str) -> String {
    format!("host-row-{key}")
}

#[component]
fn HostRow(
    host: Listed,
    selected: bool,
    on_pick: EventHandler<Listed>,
    on_enter: EventHandler<Listed>,
) -> Element {
    let entry = &host.entry;
    let sc = entry.shortcode.clone();
    let title = entry.name.clone().unwrap_or_else(|| sc.clone());
    let named = entry.name.is_some();
    let (fresh_label, fresh_ok) = freshness(entry.idle_secs);
    // Every host is reached over encrypted QUIC; what differs is whether it
    // has an address of its own or only the relay to be introduced through.
    let direct = entry
        .transport_addrs
        .iter()
        .any(|a| a.parse::<std::net::SocketAddr>().is_ok());
    let stripe = if direct {
        "#8fb0ff"
    } else {
        "var(--violet, #b98cff)"
    };
    let row_cls = if selected {
        "border-[var(--accent)] bg-[var(--accent-soft)]"
    } else {
        "border-[var(--border)] hover:border-[var(--border-strong)]"
    };
    let mark = initials(&title);
    let where_ = entry.location.map(describe);
    let via = rendezvous_label(&host.rendezvous);
    let for_pick = host.clone();
    let for_enter = host.clone();
    rsx! {
        div {
            id: host_row_id(&host.key()),
            class: "flex items-center gap-3 pl-2.5 pr-2.5 py-2 rounded-lg border transition-colors {row_cls}",
            style: "border-left: 3px solid {stripe}; min-height: {HOST_ROW_HEIGHT};",
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
                    span {
                        class: "block truncate text-[9px] font-mono uppercase tracking-wider text-[var(--text-dim)]",
                        title: "Listed by {host.rendezvous}",
                        if direct { "{fresh_label} \u{b7} direct" } else { "{fresh_label} \u{b7} relayed" }
                        if let Some(w) = where_ { " \u{b7} {w}" }
                        span { style: "text-transform: none; letter-spacing: normal;", " \u{b7} via {via}" }
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
    use super::{JoinBy, Mode, compose_rendezvous, initial_mode, join_by, merge};
    use crate::protocol::rendezvous::DiscoverEntry;

    const NONE: &[String] = &[];

    fn r() -> Vec<String> {
        vec!["ws://rendezvous.example:7700".into()]
    }

    #[test]
    fn a_launch_opens_on_the_half_that_costs_nothing() {
        assert_eq!(initial_mode(), Mode::Join);
    }

    #[test]
    fn an_untouched_address_does_not_beat_a_code() {
        assert_eq!(join_by("", "purple-fox-42", &r()), JoinBy::Code);
    }

    #[test]
    fn a_typed_address_beats_a_code() {
        assert_eq!(
            join_by("ws://box.local:9000", "purple-fox-42", &r()),
            JoinBy::Url
        );
    }

    #[test]
    fn blank_space_is_not_an_address() {
        assert_eq!(join_by("   ", "purple-fox-42", &r()), JoinBy::Code);
    }

    #[test]
    fn a_code_without_a_directory_is_not_submittable() {
        assert_eq!(join_by("", "purple-fox-42", NONE), JoinBy::Nothing);
    }

    #[test]
    fn an_empty_form_has_nothing_to_submit() {
        assert_eq!(join_by("", "", &r()), JoinBy::Nothing);
    }

    fn entry(code: &str) -> DiscoverEntry {
        DiscoverEntry {
            shortcode: code.into(),
            name: None,
            description: None,
            idle_secs: 0,
            transport_key: None,
            transport_addrs: Vec::new(),
            relay_url: None,
            location: None,
        }
    }

    #[test]
    fn the_add_form_fills_in_what_was_left_out() {
        assert_eq!(
            compose_rendezvous("r.example", false, "7700").as_deref(),
            Some("ws://r.example:7700")
        );
        assert_eq!(
            compose_rendezvous("r.example", true, "").as_deref(),
            Some("wss://r.example")
        );
        assert_eq!(
            compose_rendezvous("wss://r.example:8443", false, "7700").as_deref(),
            Some("wss://r.example:8443")
        );
        assert_eq!(compose_rendezvous("r.example", false, "0"), None);
        assert_eq!(compose_rendezvous("", false, "7700"), None);
    }

    #[test]
    fn one_code_on_two_rendezvous_is_two_hosts() {
        let (hosts, failed) = merge(vec![
            ("ws://a".into(), Ok(vec![entry("fox"), entry("owl")])),
            ("ws://down".into(), Err("fetch: refused".into())),
            ("ws://b".into(), Ok(vec![entry("fox")])),
        ]);
        let keys: Vec<String> = hosts.iter().map(|h| h.key()).collect();
        assert_eq!(keys, ["ws://a fox", "ws://a owl", "ws://b fox"]);
        assert_eq!(
            failed,
            [("ws://down".to_string(), "fetch: refused".to_string())]
        );
    }
}
