//! The forms for arriving somewhere, or being the somewhere.
//!
//! These used to be one screen you passed through before the app would start.
//! They are components now, mounted inside home, because home works without a
//! gateway and so nothing needs to be answered before it will open — see
//! `features::home`.

use dioxus::prelude::*;

use crate::identity::Identity;
use crate::state::SessionMode;

const INPUT_SM: &str = "w-full bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs text-[var(--text)] focus:outline-none focus:border-[var(--accent)] transition-colors";
const LABEL: &str = "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]";

/// Connect straight to a gateway URL, ignoring the directory entirely.
///
/// Its own form with its own button, which is what keeps the old ambiguity
/// from coming back: when the address and the code shared one submit, an
/// untouched address field with a `ws://localhost:9000` *placeholder* still
/// won, and joining by code became unreachable while looking fine. Two forms
/// cannot disagree about which one you pressed.
#[component]
pub fn AddressForm(on_go: EventHandler<SessionMode>) -> Element {
    let mut server_url = use_signal(String::new);

    let go = move || {
        let url = server_url().trim().to_string();
        if url.is_empty() {
            return;
        }
        on_go.call(SessionMode::Remote { server_url: url });
    };

    rsx! {
        form {
            class: "space-y-1.5",
            onsubmit: move |_| go(),
            label { class: LABEL, "Server address" }
            input {
                class: INPUT_SM,
                r#type: "text",
                placeholder: "ws://localhost:9000",
                value: "{server_url}",
                oninput: move |e| server_url.set(e.value()),
            }
            div { class: "text-[10px] text-[var(--text-dim)] leading-relaxed",
                "Connects straight to a gateway, ignoring the directory above."
            }
            button {
                r#type: "submit",
                class: "px-3 py-1.5 rounded text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors disabled:opacity-40",
                disabled: server_url().trim().is_empty(),
                "Connect to this address"
            }
        }
    }
}

/// Run the server on this machine.
///
/// The consequential half of arriving anywhere: it can reserve a name, publish
/// you to a public list, and make a home address dialable. That is why it is
/// folded shut rather than sitting open beside the directory.
#[component]
pub fn HostForm(on_go: EventHandler<SessionMode>) -> Element {
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let mut allow_lan = use_signal(|| false);
    let mut publish_to_rendezvous = use_signal(|| true);
    let mut publish_name = use_signal(String::new);
    let mut description = use_signal(String::new);
    let mut publish_public = use_signal(|| true);

    let go = move || {
        let rendezvous = settings.read().active_rendezvous();
        let r_url = if publish_to_rendezvous() && !rendezvous.trim().is_empty() {
            Some(rendezvous.trim().to_string())
        } else {
            None
        };
        let pn = publish_name().trim().to_string();
        let desc = description().trim().to_string();
        on_go.call(SessionMode::SelfHost {
            allow_lan: allow_lan(),
            rendezvous_url: r_url,
            publish_name: if pn.is_empty() { None } else { Some(pn) },
            description: if desc.is_empty() { None } else { Some(desc) },
            publish_public: publish_to_rendezvous() && publish_public(),
        });
    };

    rsx! {
        form {
            class: "border border-[var(--border)] rounded p-3 text-xs space-y-3",
            onsubmit: move |_| go(),
            p { class: "text-[var(--text-muted)]",
                // Not "your machine runs the voice SFU": a rendezvous that has
                // its own wins and the bundled one is never started, so the old
                // sentence claimed the opposite of what happens in the case it
                // described. See host.rs.
                "Your machine runs the server and keeps its history. Voice runs here too, unless the rendezvous supplies its own."
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
                // Outside the label, or clicking the hint would toggle the
                // checkbox it explains.
                span {
                    class: "w-4 h-4 shrink-0 flex items-center justify-center rounded-full border border-[var(--border)] text-[9px] text-[var(--text-dim)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors cursor-help",
                    // Gateway binds loopback without this, so it governs port
                    // mapping too, not just LAN.
                    title: "Friends here reach you directly, and Discordia asks your router (UPnP / NAT-PMP) to let in friends elsewhere. Your home IP becomes visible to anyone who joins that way.",
                    "?"
                }
            }
            if publish_to_rendezvous() {
                div { class: "pl-3 border-l border-[var(--border)] space-y-2",
                    div { class: "space-y-1",
                        label { class: LABEL, "Server name" }
                        input {
                            // Rendezvous canonicalizes names to lowercase on
                            // registration/lookup, so `MiServidor` resolves as
                            // `miservidor`.
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
            button {
                r#type: "submit",
                class: "dxf-cta w-full py-2 rounded text-xs",
                "Launch it  \u{2192}"
            }
        }
    }
}

/// Saved rendezvous servers as a pick-list, with add/remove. Replaces the
/// three separate "advanced" URL boxes — the address is a thing you keep, not
/// something to retype per tab.
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
                    // Inline colors as well as classes: the webview renders a
                    // native listbox, which ignores the Tailwind background on
                    // the popup and would otherwise draw dark text on dark.
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
                            // Truncates on input because `maxlength` cannot
                            // express the protocol's signing-time limit (see
                            // `protocol::truncate_username`).
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
