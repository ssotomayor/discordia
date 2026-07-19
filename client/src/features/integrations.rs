//! "Integrations" dialog (requires `ManageGuild`): install/uninstall bots.
//!
//! A bot is installed by its secp256k1 **pubkey** (64 hex chars, Nostr
//! format) — the same identity primitive users have. The installer grants it
//! a set of **permissions** (what it may do)
//! and **intents** (what events it receives). Privileged intents — message
//! *content* and the member roster — are flagged distinctly, mirroring
//! Discord's privileged-intents design: by default a bot learns that a message
//! happened, not what it said.

use dioxus::prelude::*;

use crate::identity::truncate_pubkey;
use crate::protocol::{BotInstall, ClientMessage, Id, Intent, Permission};
use crate::state::{use_app_state, use_gateway};

#[component]
pub fn IntegrationsDialog(guild_id: Id, on_close: EventHandler<()>) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

    // Pull the current installs for this guild whenever they refresh.
    let installs: Vec<BotInstall> = use_memo(move || {
        state
            .read()
            .integrations
            .get(&guild_id)
            .cloned()
            .unwrap_or_default()
    })();

    // Ask the server for this guild's installs once, on open.
    {
        let gw = gateway.clone();
        use_hook(move || gw.send(ClientMessage::FetchIntegrations { guild_id }));
    }

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: move |_| on_close.call(()),
            div {
                class: "dxf-modal-in w-[26rem] max-h-[80vh] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                    h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", "Integrations" }
                    button {
                        class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                        onclick: move |_| on_close.call(()),
                        "✕"
                    }
                }
                div { class: "flex-1 overflow-y-auto p-3 space-y-4",
                    // Installed bots.
                    div {
                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                            "Installed"
                        }
                        if installs.is_empty() {
                            div { class: "text-xs text-[var(--text-dim)] py-2",
                                "No bots installed. Add one below by its public key."
                            }
                        }
                        for bot in installs.iter().cloned() {
                            InstalledRow { key: "{bot.bot_pubkey}", guild_id, bot }
                        }
                    }
                    // Install form.
                    InstallForm { guild_id }
                }
            }
        }
    }
}

#[component]
fn InstalledRow(guild_id: Id, bot: BotInstall) -> Element {
    let gateway = use_gateway();
    let pk = bot.bot_pubkey.clone();

    rsx! {
        div { class: "border border-[var(--border)] rounded-md p-2.5 flex flex-col gap-1.5",
            div { class: "flex items-center gap-2",
                span { class: "text-sm text-[var(--text)] font-medium truncate flex-1", "{bot.name}" }
                button {
                    class: "text-[10px] uppercase tracking-wider text-[var(--danger)] border border-[var(--danger)]/40 rounded px-2 py-0.5 hover:bg-[var(--danger)]/10 transition-colors",
                    onclick: move |_| gateway.send(ClientMessage::UninstallBot {
                        guild_id,
                        bot_pubkey: pk.clone(),
                    }),
                    "Remove"
                }
            }
            div { class: "font-mono text-[10px] text-[var(--text-dim)] truncate",
                title: "{bot.bot_pubkey}",
                "{truncate_pubkey(&bot.bot_pubkey)}"
            }
            div { class: "flex flex-wrap gap-1",
                for p in bot.permissions.iter().copied() {
                    span { class: "text-[9px] px-1.5 py-px rounded bg-white/[0.04] text-[var(--text-muted)]",
                        "{p.label()}"
                    }
                }
                for i in bot.intents.iter().copied() {
                    span {
                        class: if i.is_privileged() {
                            "text-[9px] px-1.5 py-px rounded bg-[var(--warn)]/15 text-[var(--warn)]"
                        } else {
                            "text-[9px] px-1.5 py-px rounded bg-[var(--accent-soft)] text-[var(--accent)]"
                        },
                        "{i.label()}"
                    }
                }
            }
        }
    }
}

#[component]
fn InstallForm(guild_id: Id) -> Element {
    let gateway = use_gateway();
    let mut pubkey = use_signal(String::new);
    let mut name = use_signal(String::new);
    // Sensible defaults: can post, sees that messages happen (but not content).
    let mut perms = use_signal(|| vec![Permission::SendMessages]);
    let mut intents = use_signal(|| vec![Intent::GuildMessages]);
    let mut error = use_signal(|| None::<String>);

    let mut submit = move || {
        let pk = pubkey().trim().to_string();
        if pk.is_empty() {
            error.set(Some("Enter the bot's public key.".into()));
            return;
        }
        let nm = {
            let n = name().trim().to_string();
            if n.is_empty() { "Bot".to_string() } else { n }
        };
        gateway.send(ClientMessage::InstallBot {
            guild_id,
            bot_pubkey: pk,
            name: nm,
            permissions: perms(),
            intents: intents(),
        });
        // Clear for the next one; the list refreshes via GuildIntegrations.
        pubkey.set(String::new());
        name.set(String::new());
        perms.set(vec![Permission::SendMessages]);
        intents.set(vec![Intent::GuildMessages]);
        error.set(None);
    };

    rsx! {
        div { class: "border-t border-[var(--border)] pt-3",
            div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                "Install a bot"
            }
            div { class: "space-y-2",
                input {
                    class: "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs font-mono text-[var(--text)] outline-none transition-colors",
                    placeholder: "Bot public key (64 hex chars)",
                    value: "{pubkey}",
                    oninput: move |e| pubkey.set(e.value()),
                }
                input {
                    class: "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors",
                    placeholder: "Display name (e.g. PingBot)",
                    value: "{name}",
                    maxlength: 32,
                    oninput: move |e| name.set(e.value()),
                }

                // Permissions — only the bot-installable subset; management
                // permissions are human-only (ManageMessages is the exception:
                // it lets announcement bots post in read-only channels).
                div { class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] pt-1", "Permissions" }
                for p in Permission::BOT_INSTALLABLE.iter().copied() {
                    label { class: "flex items-center gap-2 text-xs text-[var(--text)] cursor-pointer select-none",
                        input {
                            r#type: "checkbox",
                            checked: perms.read().contains(&p),
                            onchange: move |_| {
                                let mut v = perms.write();
                                if let Some(i) = v.iter().position(|x| *x == p) { v.remove(i); }
                                else { v.push(p); }
                            },
                        }
                        "{p.label()}"
                    }
                }

                // Intents (privileged ones flagged).
                div { class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] pt-1", "Events (intents)" }
                for i in Intent::ALL.iter().copied() {
                    label { class: "flex items-center gap-2 text-xs text-[var(--text)] cursor-pointer select-none",
                        input {
                            r#type: "checkbox",
                            checked: intents.read().contains(&i),
                            onchange: move |_| {
                                let mut v = intents.write();
                                if let Some(idx) = v.iter().position(|x| *x == i) { v.remove(idx); }
                                else { v.push(i); }
                            },
                        }
                        span { "{i.label()}" }
                        if i.is_privileged() {
                            span { class: "text-[8px] px-1 py-px rounded bg-[var(--warn)]/15 text-[var(--warn)] uppercase tracking-wider font-semibold",
                                "Privileged"
                            }
                        }
                    }
                }

                if let Some(err) = error() {
                    div { class: "text-[11px] text-[var(--danger)]", "{err}" }
                }
                button {
                    class: "w-full mt-1 rounded px-2 py-1.5 text-[11px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                    onclick: move |_| submit(),
                    "Install"
                }
            }
        }
    }
}
