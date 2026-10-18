use dioxus::prelude::*;

use crate::host::HostInfo;
use crate::protocol::{ClientMessage, GuildVisibility, Id, Permission};
use crate::state::{AppState, SessionMode, use_app_state, use_gateway};

pub fn can_invite(state: &AppState, guild_id: Id) -> bool {
    state.can(guild_id, Permission::CreateInvite) || state.can(guild_id, Permission::ManageGuild)
}

/// What a friend types into "Add a server" to reach this one, best first. A
/// loopback URL only works on the machine that dialled it, so it is never offered.
pub fn server_addresses(mode: &SessionMode, host: Option<&HostInfo>) -> Vec<String> {
    match mode {
        SessionMode::Remote { server_url } => {
            let url = server_url.trim();
            (!is_loopback(url))
                .then(|| url.to_string())
                .into_iter()
                .collect()
        }
        SessionMode::ByCode { code, .. } => vec![code.trim().to_string()],
        SessionMode::SelfHost { .. } => host
            .map(|h| h.shortcode.iter().chain(h.share.iter()).cloned().collect())
            .unwrap_or_default(),
    }
}

fn is_loopback(url: &str) -> bool {
    let Some(host) = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
    else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[component]
pub fn InviteDialog(guild_id: Id, on_close: EventHandler<()>) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let mode = try_use_context::<SessionMode>();
    let mut copied = use_signal(|| None::<&'static str>);
    use_hook(move || {
        gateway.send(ClientMessage::CreateInvite {
            guild_id,
            rotate: false,
            expires_in_secs: None,
            max_uses: None,
        });
    });

    let s = state.read();
    let guild = s.guilds.iter().find(|g| g.id == guild_id);
    let name = guild.map(|g| g.name.clone()).unwrap_or_default();
    let public = guild.is_some_and(|g| g.visibility == GuildVisibility::Public);
    let code = s.invites.get(&guild_id).cloned();
    let addresses = mode
        .as_ref()
        .map(|m| server_addresses(m, s.host_info.as_ref()))
        .unwrap_or_default();
    let self_hosted = matches!(mode, Some(SessionMode::SelfHost { .. }));
    drop(s);

    let message = match (addresses.first(), &code) {
        (Some(address), Some(code)) => Some(crate::i18n::text(
            "Join {0} on Discordia:\n1. Add the server: {1}\n2. Join the guild with the invite code: {2}",
            &[name.clone(), address.clone(), code.clone()],
        )),
        _ => None,
    };
    let mut copy = move |what: &'static str, text: &str| {
        if crate::clipboard::copy_text(text) {
            copied.set(Some(what));
        }
    };

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: move |_| on_close.call(()),
            div {
                class: "dxf-modal-in w-[22rem] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                    h3 { class: "text-sm font-medium text-[var(--text)] flex-1 truncate",
                        {crate::i18n::text("Invite people to {0}", std::slice::from_ref(&name))}
                    }
                    button {
                        class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                        onclick: move |_| on_close.call(()),
                        "✕"
                    }
                }
                div { class: "px-4 py-3 flex flex-col gap-3",
                    div { class: "flex flex-col gap-1.5",
                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]",
                            {crate::i18n::text("1. Server address", &[])}
                        }
                        if addresses.is_empty() {
                            p { class: "text-[11px] text-[var(--warn)] leading-relaxed",
                                if self_hosted {
                                    {crate::i18n::text("Only this machine can reach this server. Turn on direct connections or publish it to a rendezvous from the home screen.", &[])}
                                } else {
                                    {crate::i18n::text("You reached this server on this machine only. Ask its operator for the address friends use.", &[])}
                                }
                            }
                        }
                        for (i, address) in addresses.iter().cloned().enumerate() {
                            CopyRow {
                                key: "{address}",
                                value: address.clone(),
                                copied: copied() == Some(if i == 0 { "address" } else { "share" }),
                                on_copy: move |_| copy(if i == 0 { "address" } else { "share" }, &address),
                            }
                        }
                        p { class: "text-[10.5px] text-[var(--text-dim)] leading-relaxed",
                            {crate::i18n::text("They paste it into Add a server, beside the guild list.", &[])}
                        }
                    }
                    div { class: "flex flex-col gap-1.5",
                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]",
                            {crate::i18n::text("2. Invite code", &[])}
                        }
                        match code.clone() {
                            Some(code) => rsx! {
                                CopyRow {
                                    value: code.clone(),
                                    copied: copied() == Some("code"),
                                    on_copy: move |_| copy("code", &code),
                                }
                            },
                            None => rsx! {
                                p { class: "text-[11px] text-[var(--text-dim)]", {crate::i18n::text("Fetching the invite code…", &[])} }
                            },
                        }
                        p { class: "text-[10.5px] text-[var(--text-dim)] leading-relaxed",
                            {crate::i18n::text("Then the + below the guilds → Explore → Have an invite code?", &[])}
                            if public {
                                {crate::i18n::text(" This guild is public, so they can also find it under Explore.", &[])}
                            }
                        }
                    }
                }
                div { class: "px-4 py-3 border-t border-[var(--border)] flex items-center justify-end gap-2",
                    button {
                        class: "px-3 py-1 rounded text-[11px] text-[var(--text-muted)] hover:text-[var(--text)] transition-colors",
                        onclick: move |_| on_close.call(()),
                        {crate::i18n::text("Close", &[])}
                    }
                    if let Some(message) = message {
                        button {
                            class: "px-3 py-1 rounded text-[11px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                            title: crate::i18n::text("Copies both steps as one message to send", &[]),
                            onclick: move |_| copy("message", &message),
                            if copied() == Some("message") { {crate::i18n::text("Copied", &[])} } else { {crate::i18n::text("Copy invite message", &[])} }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn CopyRow(value: String, copied: bool, on_copy: EventHandler<()>) -> Element {
    rsx! {
        div { class: "flex items-center gap-2 bg-[var(--panel2)] border border-[var(--border)] rounded-md pl-2.5 pr-1 py-1",
            code { class: "flex-1 min-w-0 truncate font-mono text-[12px] text-[var(--text)] select-all", title: "{value}", "{value}" }
            button {
                class: "w-6 h-6 shrink-0 flex items-center justify-center rounded text-[var(--text-dim)] hover:text-[var(--accent)] transition-colors",
                title: if copied { crate::i18n::text("Copied", &[]) } else { crate::i18n::text("Copy", &[]) },
                onclick: move |_| on_copy.call(()),
                span {
                    class: "block w-3.5 h-3.5",
                    dangerous_inner_html: if copied { super::icons::CHECK } else { super::icons::COPY },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(url: &str) -> SessionMode {
        SessionMode::Remote {
            server_url: url.into(),
        }
    }

    #[test]
    fn a_remote_server_offers_the_address_it_was_dialled_at() {
        assert_eq!(
            server_addresses(&remote(" wss://app.discordia.world "), None),
            vec!["wss://app.discordia.world".to_string()]
        );
        assert_eq!(
            server_addresses(&remote("quic://abc@203.0.113.7:9001"), None),
            vec!["quic://abc@203.0.113.7:9001".to_string()]
        );
    }

    #[test]
    fn a_loopback_address_is_never_offered() {
        for url in [
            "ws://127.0.0.1:9000",
            "ws://localhost:9000",
            "ws://[::1]:9000",
        ] {
            assert!(server_addresses(&remote(url), None).is_empty(), "{url}");
        }
    }

    #[test]
    fn a_code_is_offered_as_the_code() {
        let mode = SessionMode::ByCode {
            rendezvous_url: "ws://rendezvous.discordia.world:7700".into(),
            code: "purple-fox-42".into(),
        };
        assert_eq!(
            server_addresses(&mode, None),
            vec!["purple-fox-42".to_string()]
        );
    }
}
