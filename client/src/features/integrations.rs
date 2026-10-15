use dioxus::prelude::*;

use crate::identity::{npub_of, pubkey_from_input, truncate_pubkey};
use crate::nostr::event::Event;
use crate::nostr::service::{NostrCmd, NostrTx};
use crate::protocol::{
    APP_MANIFEST_D, APP_MANIFEST_KIND, AppManifest, BotInstall, ClientMessage, Id, Intent,
    Permission, validate_manifest,
};
use crate::state::{use_app_state, use_gateway};

/// Checked here as well as on the host: the consent screen must not draw a
/// manifest the key never signed.
pub fn read_manifest(json: &str, author: &str) -> Result<AppManifest, String> {
    let ev: Event = serde_json::from_str(json).map_err(|e| format!("not an event: {e}"))?;
    if ev.pubkey != author {
        return Err("signed by a different key".into());
    }
    if u32::from(ev.kind) != APP_MANIFEST_KIND || ev.tag("d") != Some(APP_MANIFEST_D) {
        return Err("not an app manifest".into());
    }
    if !ev.verify() {
        return Err("the signature does not verify".into());
    }
    let raw: AppManifest =
        serde_json::from_str(&ev.content).map_err(|e| format!("unreadable manifest: {e}"))?;
    validate_manifest(raw)
}

/// What `newer` asks for that `pinned` was never granted, in words.
pub fn asks_more(pinned: &AppManifest, newer: &AppManifest) -> Vec<String> {
    let mut out: Vec<String> = newer
        .permissions
        .iter()
        .filter(|p| !pinned.permissions.contains(p))
        .map(|p| crate::i18n::label(p.label()).to_string())
        .collect();
    out.extend(
        newer
            .intents
            .iter()
            .filter(|i| !pinned.intents.contains(i))
            .map(|i| crate::i18n::label(i.label()).to_string()),
    );
    let caps = |m: &AppManifest| {
        m.activity
            .as_ref()
            .map(|a| a.capabilities.clone())
            .unwrap_or_default()
    };
    let old_caps = caps(pinned);
    out.extend(
        caps(newer)
            .into_iter()
            .filter(|c| !old_caps.contains(c))
            .map(|c| crate::i18n::label(c.label()).to_string()),
    );
    let players = |m: &AppManifest| m.activity.as_ref().map_or(1, |a| a.max_players);
    if players(newer) > players(pinned) {
        out.push(format!("Up to {} players", players(newer)));
    }
    out
}

#[component]
pub fn IntegrationsDialog(guild_id: Id, on_close: EventHandler<()>) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

    let installs: Vec<BotInstall> = use_memo(move || {
        state
            .read()
            .integrations
            .get(&guild_id)
            .cloned()
            .unwrap_or_default()
    })();

    {
        let gw = gateway.clone();
        use_hook(move || {
            gw.send(ClientMessage::FetchIntegrations { guild_id });
            gw.send(ClientMessage::FetchGuildActivities { guild_id });
        });
    }

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: move |_| on_close.call(()),
            div {
                class: "dxf-modal-in w-[26rem] max-h-[80vh] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                    h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", {crate::i18n::text("Integrations", &[])} }
                    button {
                        class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                        onclick: move |_| on_close.call(()),
                        "✕"
                    }
                }
                div { class: "flex-1 overflow-y-auto p-3 space-y-4",
                    div {
                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                            {crate::i18n::text("Installed", &[])}
                        }
                        if installs.is_empty() {
                            div { class: "text-xs text-[var(--text-dim)] py-2",
                                {crate::i18n::text("Nothing installed. Add an app below by its key.", &[])}
                            }
                        }
                        for bot in installs.iter().cloned() {
                            InstalledRow {
                                key: "{bot.bot_pubkey}",
                                guild_id,
                                others: installs.iter().filter(|o| o.bot_pubkey != bot.bot_pubkey).cloned().collect::<Vec<_>>(),
                                bot,
                            }
                        }
                    }
                    InstallForm { guild_id }
                    BlocklistsSection { guild_id }
                }
            }
        }
    }
}

#[component]
fn InstalledRow(guild_id: Id, bot: BotInstall, others: Vec<BotInstall>) -> Element {
    let gateway = use_gateway();
    let pk = bot.bot_pubkey.clone();
    let approve_pk = bot.bot_pubkey.clone();
    let approve_gw = gateway.clone();
    let blocked = blocked_by(&use_app_state().read(), guild_id, &bot.bot_pubkey);
    let update = match (&bot.app, &bot.pending) {
        (Some(pinned), Some(next)) => Some((next.version, asks_more(pinned, next))),
        _ => None,
    };

    rsx! {
        div { class: "border border-[var(--border)] rounded-md p-2.5 flex flex-col gap-1.5",
            div { class: "flex items-center gap-2",
                if let Some(app) = &bot.app {
                    span { class: "text-sm", "{app.icon}" }
                }
                span { class: "text-sm text-[var(--text)] font-medium truncate flex-1", "{bot.name}" }
                if let Some(app) = &bot.app {
                    span { class: "text-[9px] text-[var(--text-dim)] font-mono", {crate::i18n::text("v{0}", &[(app . version).to_string()])} }
                }
                button {
                    class: "text-[10px] uppercase tracking-wider text-[var(--danger)] border border-[var(--danger)]/40 rounded px-2 py-0.5 hover:bg-[var(--danger)]/10 transition-colors",
                    onclick: move |_| gateway.send(ClientMessage::UninstallBot {
                        guild_id,
                        bot_pubkey: pk.clone(),
                    }),
                    {crate::i18n::text("Remove", &[])}
                }
            }
            div { class: "font-mono text-[10px] text-[var(--text-dim)] truncate",
                title: "{bot.bot_pubkey}",
                "{truncate_pubkey(&bot.bot_pubkey)}"
            }
            div { class: "flex flex-wrap gap-1",
                for p in bot.permissions.iter().copied() {
                    span { class: "text-[9px] px-1.5 py-px rounded bg-white/[0.04] text-[var(--text-muted)]",
                        {crate::i18n::label(p.label())}
                    }
                }
                for i in bot.intents.iter().copied() {
                    span {
                        class: if i.is_privileged() {
                            "text-[9px] px-1.5 py-px rounded bg-[var(--warn)]/15 text-[var(--warn)]"
                        } else {
                            "text-[9px] px-1.5 py-px rounded bg-[var(--accent-soft)] text-[var(--accent)]"
                        },
                        {crate::i18n::label(i.label())}
                    }
                }
            }
            if let Some(curator) = blocked {
                div { class: "text-[11px] text-[var(--danger)]",
                    {crate::i18n::text("On a blocklist this guild follows ({0}). Consider removing it.", &[(truncate_pubkey (& npub_of (& curator))).to_string()])}
                }
            }
            if let Some(activity) = bot.app.as_ref().and_then(|a| a.activity.clone()) {
                BundleRow { guild_id, bot_pubkey: bot.bot_pubkey.clone(), activity }
            }
            if !others.is_empty() {
                AppCallsRow { guild_id, bot: bot.clone(), others }
            }
            if let Some((version, more)) = update {
                div { class: "rounded border border-[var(--warn)]/40 bg-[var(--warn)]/15 p-2 space-y-1",
                    div { class: "text-[11px] font-semibold text-[var(--warn)]",
                        {crate::i18n::text("Version {0} asks for more:", &[(version).to_string()])}
                    }
                    for line in more.iter() {
                        div { class: "text-[11px] text-[var(--text)]", "• {line}" }
                    }
                    button {
                        class: "w-full mt-1 rounded px-2 py-1 text-[10px] uppercase tracking-wider text-[var(--warn)] border border-[var(--warn)]/40 hover:bg-[var(--warn)]/10 transition-colors",
                        onclick: move |_| approve_gw.send(ClientMessage::ApproveAppUpdate {
                            guild_id,
                            bot_pubkey: approve_pk.clone(),
                        }),
                        {crate::i18n::text("Approve update", &[])}
                    }
                }
            }
        }
    }
}

#[component]
fn ManualInstall(guild_id: Id) -> Element {
    let gateway = use_gateway();
    let mut pubkey = use_signal(String::new);
    let mut name = use_signal(String::new);
    let mut perms = use_signal(|| vec![Permission::SendMessages]);
    let mut intents = use_signal(|| vec![Intent::GuildMessages]);
    let mut error = use_signal(|| None::<String>);
    let mut confirming = use_signal(|| false);

    let mut submit = move || {
        let pk = pubkey().trim().to_string();
        if pk.is_empty() {
            error.set(Some("Enter the bot's public key.".into()));
            return;
        }
        if !confirming() && intents().iter().any(|i| i.is_privileged()) {
            confirming.set(true);
            error.set(None);
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
            manifest: None,
        });
        pubkey.set(String::new());
        name.set(String::new());
        perms.set(vec![Permission::SendMessages]);
        intents.set(vec![Intent::GuildMessages]);
        error.set(None);
        confirming.set(false);
    };

    rsx! {
        div { class: "border-t border-[var(--border)] pt-3",
            div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                {crate::i18n::text("Install without a manifest", &[])}
            }
            div { class: "space-y-2",
                input {
                    class: "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs font-mono text-[var(--text)] outline-none transition-colors",
                    placeholder: crate::i18n::text("Bot public key (64 hex chars)", &[]),
                    value: "{pubkey}",
                    oninput: move |e| pubkey.set(e.value()),
                }
                input {
                    class: "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors",
                    placeholder: crate::i18n::text("Display name (e.g. PingBot)", &[]),
                    value: "{name}",
                    maxlength: 32,
                    oninput: move |e| name.set(e.value()),
                }

                div { class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] pt-1", {crate::i18n::text("Permissions", &[])} }
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
                        {crate::i18n::label(p.label())}
                    }
                }

                div { class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] pt-1", {crate::i18n::text("Events (intents)", &[])} }
                for i in Intent::ALL.iter().copied() {
                    label { class: "flex items-center gap-2 text-xs text-[var(--text)] cursor-pointer select-none",
                        input {
                            r#type: "checkbox",
                            checked: intents.read().contains(&i),
                            onchange: move |_| {
                                {
                                    let mut v = intents.write();
                                    if let Some(idx) = v.iter().position(|x| *x == i) { v.remove(idx); }
                                    else { v.push(i); }
                                }
                                confirming.set(false);
                            },
                        }
                        span { {crate::i18n::label(i.label())} }
                        if i.is_privileged() {
                            span { class: "text-[8px] px-1 py-px rounded bg-[var(--warn)]/15 text-[var(--warn)] uppercase tracking-wider font-semibold",
                                {crate::i18n::text("Privileged", &[])}
                            }
                        }
                    }
                }

                if let Some(err) = error() {
                    div { class: "text-[11px] text-[var(--danger)]", {crate::i18n::message(&(err).to_string())} }
                }

                if confirming() {
                    div { class: "rounded border border-[var(--warn)]/40 bg-[var(--warn)]/15 p-2 space-y-1",
                        div { class: "text-[11px] font-semibold text-[var(--warn)]",
                            {crate::i18n::text("This bot will be able to read:", &[])}
                        }
                        for i in intents().iter().copied().filter(|i| i.is_privileged()) {
                            div { class: "text-[11px] text-[var(--text)]",
                                match i {
                                    Intent::MessageContent => crate::i18n::text("• the text and images of every message in this guild", &[]),
                                    Intent::Members => crate::i18n::text("• who joins and who leaves this guild", &[]),
                                    _ => "• —".to_owned(),
                                }
                            }
                        }
                        div { class: "text-[10px] text-[var(--text-muted)]",
                            {crate::i18n::text("Uninstalling later stops the delivery, not what was already read.", &[])}
                        }
                    }
                }

                button {
                    class: if confirming() {
                        "w-full mt-1 rounded px-2 py-1.5 text-[11px] uppercase tracking-wider text-[var(--warn)] border border-[var(--warn)]/40 transition-colors"
                    } else {
                        "w-full mt-1 rounded px-2 py-1.5 text-[11px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors"
                    },
                    onclick: move |_| submit(),
                    if confirming() { {crate::i18n::text("Grant and install", &[])} } else { {crate::i18n::text("Install", &[])} }
                }
                if confirming() {
                    button {
                        class: "w-full rounded px-2 py-1 text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                        onclick: move |_| confirming.set(false),
                        {crate::i18n::text("Cancel", &[])}
                    }
                }
            }
        }
    }
}

#[component]
fn InstallForm(guild_id: Id) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let nostr = try_use_context::<NostrTx>();
    let mut input = use_signal(String::new);
    let mut looked_up = use_signal(|| None::<String>);
    let mut error = use_signal(|| None::<String>);
    let mut manual = use_signal(|| false);

    let lookup_gw = gateway.clone();
    let look_up = use_callback(move |_: ()| match pubkey_from_input(&input()) {
        Ok(key) => {
            error.set(None);
            lookup_gw.send(ClientMessage::FetchAppManifest {
                bot_pubkey: key.clone(),
            });
            if let Some(n) = &nostr {
                n.send(NostrCmd::FetchAppManifest {
                    author: key.clone(),
                });
            }
            looked_up.set(Some(key));
        }
        Err(e) => error.set(Some(e)),
    });

    let found = use_memo(move || {
        let key = looked_up()?;
        let s = state.read();
        let host = s.app_manifest_events.get(&key).cloned();
        let relay = s.relay_app_manifests.get(&key).map(|(_, j)| j.clone());
        let mut best: Option<(AppManifest, String)> = None;
        let mut problem = None;
        for json in [host.clone().flatten(), relay.clone()]
            .into_iter()
            .flatten()
        {
            match read_manifest(&json, &key) {
                Ok(m) if best.as_ref().is_none_or(|(b, _)| m.version > b.version) => {
                    best = Some((m, json))
                }
                Ok(_) => {}
                Err(e) => problem = Some(e),
            }
        }
        let host_answered = host.is_some();
        Some((key, best, problem, host_answered))
    });

    rsx! {
        div { class: "border-t border-[var(--border)] pt-3 space-y-2",
            div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]",
                {crate::i18n::text("Install an app", &[])}
            }
            div { class: "flex gap-2",
                input {
                    class: "flex-1 min-w-0 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs font-mono text-[var(--text)] outline-none transition-colors",
                    placeholder: crate::i18n::text("npub1… or hex key", &[]),
                    value: "{input}",
                    oninput: move |e| { input.set(e.value()); looked_up.set(None); },
                    onkeydown: move |e| if e.key() == Key::Enter { look_up.call(()); },
                }
                button {
                    class: "rounded px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                    onclick: move |_| look_up.call(()),
                    {crate::i18n::text("Look up", &[])}
                }
            }
            if let Some(err) = error() {
                div { class: "text-[11px] text-[var(--danger)]", {crate::i18n::message(&(err).to_string())} }
            }
            if let Some((key, best, problem, host_answered)) = found() {
                match best {
                    Some((manifest, event)) => rsx! {
                        AppConsent {
                            key: "{key}",
                            guild_id,
                            bot_pubkey: key.clone(),
                            manifest,
                            event,
                            on_done: move |_| { input.set(String::new()); looked_up.set(None); },
                        }
                    },
                    None => rsx! {
                        div { class: "text-[11px] text-[var(--text-dim)]",
                            if let Some(p) = problem {
                                {crate::i18n::text("That key's manifest was refused: {0}", std::slice::from_ref(&(p)))}
                            } else if host_answered {
                                {crate::i18n::text("Looking on your relays… This host has never seen that app's manifest.", &[])}
                            } else {
                                {crate::i18n::text("Looking…", &[])}
                            }
                        }
                    },
                }
            }
            button {
                class: "text-[10px] text-[var(--text-dim)] hover:text-[var(--text-muted)] underline",
                onclick: move |_| manual.set(!manual()),
                if manual() { {crate::i18n::text("Hide manual install", &[])} } else { {crate::i18n::text("Install a bot without a manifest", &[])} }
            }
            if manual() {
                ManualInstall { guild_id }
            }
        }
    }
}

#[component]
fn AppConsent(
    guild_id: Id,
    bot_pubkey: String,
    manifest: AppManifest,
    event: String,
    on_done: EventHandler<()>,
) -> Element {
    let gateway = use_gateway();
    let state = use_app_state();
    let blocked = blocked_by(&state.read(), guild_id, &bot_pubkey);
    let privileged = manifest.intents.iter().any(|i| i.is_privileged());
    let npub = npub_of(&bot_pubkey);
    let install = {
        let manifest = manifest.clone();
        move |_| {
            gateway.send(ClientMessage::InstallBot {
                guild_id,
                bot_pubkey: bot_pubkey.clone(),
                name: String::new(),
                permissions: manifest.permissions.clone(),
                intents: manifest.intents.clone(),
                manifest: Some(event.clone()),
            });
            on_done.call(());
        }
    };
    rsx! {
        div { class: "rounded-md border border-[var(--border)] p-2.5 space-y-2",
            div { class: "flex items-center gap-2",
                span { class: "text-lg", "{manifest.icon}" }
                div { class: "flex-1 min-w-0",
                    div { class: "text-sm text-[var(--text)] font-medium truncate", "{manifest.name}" }
                    div { class: "font-mono text-[10px] text-[var(--text-dim)] truncate", title: "{npub}",
                        {crate::i18n::text("{0} · v{1}", &[(truncate_pubkey (& npub)).to_string(), (manifest . version).to_string()])}
                    }
                }
            }
            if !manifest.description.is_empty() {
                div { class: "text-[11px] text-[var(--text-muted)]", "{manifest.description}" }
            }
            div { class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)]", {crate::i18n::text("It asks to", &[])} }
            ul { class: "space-y-0.5",
                for p in manifest.permissions.iter().copied() {
                    li { class: "text-[11px] text-[var(--text)]", {format!("• {}", crate::i18n::label(p.label()))} }
                }
                for i in manifest.intents.iter().copied() {
                    li { class: if i.is_privileged() { "text-[11px] text-[var(--warn)]" } else { "text-[11px] text-[var(--text)]" },
                        {crate::i18n::text("• Receive: {0}", &[(i . label ()).to_string()])}
                    }
                }
                if let Some(a) = &manifest.activity {
                    for c in a.capabilities.iter().copied() {
                        li { class: "text-[11px] text-[var(--text)]", {crate::i18n::text("• Activity: {0}", &[(c . label ()).to_string()])} }
                    }
                    if a.max_players > 1 {
                        li { class: "text-[11px] text-[var(--text)]", {crate::i18n::text("• Activity for up to {0} players", &[(a . max_players).to_string()])} }
                    }
                }
                if manifest.permissions.is_empty() && manifest.intents.is_empty() && manifest.activity.is_none() {
                    li { class: "text-[11px] text-[var(--text-dim)]", {crate::i18n::text("Nothing beyond appearing in the member list", &[])} }
                }
            }
            if !manifest.commands.is_empty() {
                div { class: "text-[10px] text-[var(--text-dim)]",
                    {crate::i18n::text("Buttons: ", &[])}
                    {manifest.commands.iter().map(|c| c.label.clone()).collect::<Vec<_>>().join(", ")}
                }
            }
            if privileged {
                div { class: "text-[10px] text-[var(--warn)]",
                    {crate::i18n::text("Highlighted items let it read what members write or who comes and goes. Uninstalling later stops the delivery, not what was already read.", &[])}
                }
            }
            div { class: "text-[10px] text-[var(--text-dim)]",
                {crate::i18n::text("A later version that asks for more waits for your approval.", &[])}
            }
            if let Some(curator) = &blocked {
                div { class: "text-[11px] text-[var(--danger)]",
                    {crate::i18n::text("This app is on a blocklist this guild follows ({0}), so it can't be installed.", &[(truncate_pubkey (& npub_of (curator))).to_string()])}
                }
            }
            button {
                disabled: blocked.is_some(),
                class: if privileged {
                    "w-full rounded px-2 py-1.5 text-[11px] uppercase tracking-wider text-[var(--warn)] border border-[var(--warn)]/40 hover:bg-[var(--warn)]/10 transition-colors"
                } else {
                    "w-full rounded px-2 py-1.5 text-[11px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors"
                },
                onclick: install,
                {crate::i18n::text("Approve and install", &[])}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nostr::event::{sign_with, xonly_hex};
    use crate::protocol::{ActivityCapability, ActivityManifest};

    fn manifest(version: u64) -> AppManifest {
        AppManifest {
            name: "RPS".into(),
            description: String::new(),
            icon: "✊".into(),
            version,
            permissions: vec![Permission::SendMessages],
            intents: vec![Intent::GuildMessages],
            commands: Vec::new(),
            activity: Some(ActivityManifest {
                sha256: "0".repeat(64),
                capabilities: vec![ActivityCapability::UserRead],
                max_players: 2,
                url: None,
            }),
        }
    }

    fn signed(secret: &secp256k1::SecretKey, kind: u16, d: &str, content: String) -> String {
        let tags = vec![vec!["d".to_string(), d.to_string()]];
        serde_json::to_string(&sign_with(secret, 1, kind, tags, content)).unwrap()
    }

    #[test]
    fn only_a_signed_manifest_from_that_key_is_read() {
        let secret = secp256k1::SecretKey::from_slice(&[7; 32]).unwrap();
        let key = xonly_hex(&secret);
        let body = serde_json::to_string(&manifest(1)).unwrap();
        let good = signed(&secret, 31990, APP_MANIFEST_D, body.clone());
        assert_eq!(read_manifest(&good, &key).unwrap().version, 1);

        assert!(read_manifest(&good, &"1".repeat(64)).is_err());
        assert!(
            read_manifest(&signed(&secret, 30078, APP_MANIFEST_D, body.clone()), &key).is_err()
        );
        assert!(read_manifest(&signed(&secret, 31990, "other", body.clone()), &key).is_err());

        let mut tampered: Event = serde_json::from_str(&good).unwrap();
        tampered.content = body.replace("\"version\":1", "\"version\":9");
        let tampered = serde_json::to_string(&tampered).unwrap();
        assert!(
            read_manifest(&tampered, &key)
                .unwrap_err()
                .contains("signature")
        );
    }

    #[test]
    fn only_a_followed_list_blocks() {
        let mut state = crate::state::AppState::empty();
        let guild = Id::new_v4();
        let list = crate::protocol::AppBlocklist {
            curator: "c".repeat(64),
            created_at: 1,
            blocked: vec!["b".repeat(64)],
        };
        state
            .guild_blocklists
            .insert(guild, (vec![list.curator.clone()], vec![list]));
        assert_eq!(
            blocked_by(&state, guild, &"b".repeat(64)),
            Some("c".repeat(64))
        );
        assert_eq!(blocked_by(&state, guild, &"a".repeat(64)), None);
        assert_eq!(blocked_by(&state, Id::new_v4(), &"b".repeat(64)), None);
    }

    #[test]
    fn an_update_names_only_what_it_adds() {
        let v1 = manifest(1);
        let mut v2 = manifest(2);
        assert!(asks_more(&v1, &v2).is_empty());
        v2.permissions.push(Permission::AddReactions);
        v2.activity
            .as_mut()
            .unwrap()
            .capabilities
            .push(ActivityCapability::MessageSend);
        v2.activity.as_mut().unwrap().max_players = 4;
        let more = asks_more(&v1, &v2);
        assert_eq!(more.len(), 3, "{more:?}");
        assert!(more.contains(&"Up to 4 players".to_string()));
    }
}

#[component]
fn BundleRow(
    guild_id: Id,
    bot_pubkey: String,
    activity: crate::protocol::ActivityManifest,
) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let held = state
        .read()
        .guild_activities
        .get(&guild_id)
        .is_some_and(|list| {
            list.iter()
                .any(|a| a.bot_pubkey == bot_pubkey && a.sha256 == activity.sha256)
        });
    let upload =
        use_callback(
            move |bytes: Vec<u8>| match bundle_data_url(&bytes, &activity.sha256) {
                Ok(data_url) => gateway.send(ClientMessage::UploadActivityBundle {
                    guild_id,
                    bot_pubkey: bot_pubkey.clone(),
                    data_url,
                }),
                Err(e) => error.set(Some(e)),
            },
        );
    let url = activity.url.clone();
    rsx! {
        div { class: "text-[11px] flex flex-col gap-1",
            if held {
                div { class: "text-[var(--text-muted)]", {crate::i18n::text("Activity ready for members", &[])} }
            } else {
                div { class: "text-[var(--warn)]", {crate::i18n::text("Waiting for the app to send its activity — its bot sends it when online.", &[])} }
                div { class: "text-[10px] text-[var(--text-dim)]", {crate::i18n::text("Or provide the file yourself:", &[])} }
                div { class: "flex gap-2",
                    button {
                        class: "rounded px-2 py-0.5 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors disabled:opacity-50",
                        disabled: busy(),
                        onclick: move |_| {
                            spawn(async move {
                                let Some(file) = rfd::AsyncFileDialog::new()
                                    .add_filter(crate::i18n::label("Activity bundle"), &["zip", "html", "htm"])
                                    .pick_file()
                                    .await
                                else {
                                    return;
                                };
                                busy.set(true);
                                error.set(None);
                                upload.call(file.read().await);
                                busy.set(false);
                            });
                        },
                        {crate::i18n::text("Upload file…", &[])}
                    }
                    if let Some(url) = url {
                        button {
                            class: "rounded px-2 py-0.5 text-[10px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors disabled:opacity-50",
                            title: crate::i18n::text("Downloads from {0} — that site sees your address. Members never do.", std::slice::from_ref(&(url))),
                            disabled: busy(),
                            onclick: move |_| {
                                let url = url.clone();
                                spawn(async move {
                                    busy.set(true);
                                    error.set(None);
                                    match download_bundle(&url).await {
                                        Ok(bytes) => upload.call(bytes),
                                        Err(e) => error.set(Some(e)),
                                    }
                                    busy.set(false);
                                });
                            },
                            {crate::i18n::text("Download from the developer", &[])}
                        }
                    }
                }
            }
            if let Some(err) = error() {
                div { class: "text-[var(--danger)]", {crate::i18n::message(&(err).to_string())} }
            }
        }
    }
}

/// Checked before upload too, so a wrong file fails here with a reason
/// rather than at the host.
fn bundle_data_url(bytes: &[u8], sha256: &str) -> Result<String, String> {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    if bytes.len() > crate::protocol::MAX_BUNDLE_BYTES {
        return Err("the bundle is over 2 MiB".into());
    }
    if hex::encode(Sha256::digest(bytes)) != sha256 {
        return Err("that file is not the bundle this version names".into());
    }
    crate::activity_bundle::unpack(bytes)?;
    Ok(format!(
        "data:{};base64,{}",
        crate::protocol::BUNDLE_MIME,
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

async fn download_bundle(url: &str) -> Result<Vec<u8>, String> {
    let response = reqwest::get(url)
        .await
        .map_err(|e| format!("download failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("download failed: {e}"))?;
    if response
        .content_length()
        .is_some_and(|n| n > crate::protocol::MAX_BUNDLE_BYTES as u64)
    {
        return Err("the bundle is over 2 MiB".into());
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("download failed: {e}"))?
    {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > crate::protocol::MAX_BUNDLE_BYTES {
            return Err("the bundle is over 2 MiB".into());
        }
    }
    Ok(bytes)
}

/// Which other apps this one may call. Each call is still held to the
/// calling app's permissions and to the person who started it.
#[component]
fn AppCallsRow(guild_id: Id, bot: BotInstall, others: Vec<BotInstall>) -> Element {
    let gateway = use_gateway();
    let mut open = use_signal(|| false);
    let allowed = bot.may_call.len();
    rsx! {
        div { class: "text-[11px]",
            button {
                class: "text-[10px] text-[var(--text-dim)] hover:text-[var(--text-muted)]",
                onclick: move |_| open.set(!open()),
                if allowed == 0 { {crate::i18n::text("May call no other app", &[])} } else { {crate::i18n::text("May call {0} other app(s)", &[(allowed).to_string()])} }
                if open() { " ▴" } else { " ▾" }
            }
            if open() {
                div { class: "mt-1 space-y-0.5",
                    for other in others.iter().cloned() {
                        label { key: "{other.bot_pubkey}", class: "flex items-center gap-2 text-xs text-[var(--text)] cursor-pointer select-none",
                            input {
                                r#type: "checkbox",
                                checked: bot.may_call.contains(&other.bot_pubkey),
                                onchange: {
                                    let gateway = gateway.clone();
                                    let bot = bot.clone();
                                    let target = other.bot_pubkey.clone();
                                    move |_| {
                                        let mut next = bot.may_call.clone();
                                        if let Some(i) = next.iter().position(|k| *k == target) {
                                            next.remove(i);
                                        } else {
                                            next.push(target.clone());
                                        }
                                        gateway.send(ClientMessage::SetAppCalls {
                                            guild_id,
                                            bot_pubkey: bot.bot_pubkey.clone(),
                                            may_call: next,
                                        });
                                    }
                                },
                            }
                            "{other.name}"
                        }
                    }
                    div { class: "text-[10px] text-[var(--text-dim)]",
                        {crate::i18n::text("A call can't do more than the calling app may, or than the person who started it may.", &[])}
                    }
                }
            }
        }
    }
}

/// The curator whose followed list names `app`, from what the host sent.
pub fn blocked_by(state: &crate::state::AppState, guild_id: Id, app: &str) -> Option<String> {
    state
        .guild_blocklists
        .get(&guild_id)?
        .1
        .iter()
        .find(|l| l.blocked.iter().any(|k| k == app))
        .map(|l| l.curator.clone())
}

/// Curators' lists are fetched here, by an admin's client, and handed to the
/// host: the host has no relays of its own to read them from.
#[component]
fn BlocklistsSection(guild_id: Id) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let nostr = try_use_context::<NostrTx>();
    let mut input = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);
    let mut handed = use_signal(std::collections::HashSet::<(String, i64)>::new);

    let (curators, lists) = state
        .read()
        .guild_blocklists
        .get(&guild_id)
        .cloned()
        .unwrap_or_default();

    let ask_relays = curators.clone();
    use_effect(use_reactive!(|ask_relays| {
        if let Some(n) = &nostr {
            n.send(NostrCmd::FetchBlocklists {
                authors: ask_relays,
            });
        }
    }));

    let upload_gw = gateway.clone();
    use_effect(move || {
        let s = state.read();
        let Some((curators, lists)) = s.guild_blocklists.get(&guild_id) else {
            return;
        };
        for curator in curators {
            let Some((at, json)) = s.relay_blocklists.get(curator) else {
                continue;
            };
            let held = lists
                .iter()
                .find(|l| &l.curator == curator)
                .map(|l| l.created_at);
            let key = (curator.clone(), *at);
            if held.is_none_or(|h| h < *at) && !handed.peek().contains(&key) {
                handed.write().insert(key);
                upload_gw.send(ClientMessage::UploadBlocklist {
                    event: json.clone(),
                });
            }
        }
    });

    let set_gw = gateway.clone();
    let follow = {
        let curators = curators.clone();
        move |_| match pubkey_from_input(&input()) {
            Ok(key) => {
                let mut next = curators.clone();
                next.push(key);
                set_gw.send(ClientMessage::SetGuildBlocklists {
                    guild_id,
                    curators: next,
                });
                input.set(String::new());
                error.set(None);
            }
            Err(e) => error.set(Some(e)),
        }
    };

    rsx! {
        div { class: "border-t border-[var(--border)] pt-3 space-y-2",
            div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]",
                {crate::i18n::text("App blocklists", &[])}
            }
            div { class: "text-[10px] text-[var(--text-dim)]",
                {crate::i18n::text("Follow someone's list of apps to refuse. Nothing is followed unless you add it here.", &[])}
            }
            for curator in curators.iter().cloned() {
                div { key: "{curator}", class: "flex items-center gap-2 text-[11px]",
                    span { class: "font-mono text-[var(--text)] truncate flex-1", title: "{npub_of(&curator)}",
                        "{truncate_pubkey(&npub_of(&curator))}"
                    }
                    span { class: "text-[var(--text-dim)]",
                        match lists.iter().find(|l| l.curator == curator) {
                            Some(l) => format!("{} app(s)", l.blocked.len()),
                            None => "not fetched yet".to_string(),
                        }
                    }
                    button {
                        class: "text-[10px] uppercase tracking-wider text-[var(--danger)] hover:underline",
                        onclick: {
                            let gateway = gateway.clone();
                            let rest: Vec<String> = curators.iter().filter(|c| **c != curator).cloned().collect();
                            move |_| gateway.send(ClientMessage::SetGuildBlocklists { guild_id, curators: rest.clone() })
                        },
                        {crate::i18n::text("Unfollow", &[])}
                    }
                }
            }
            div { class: "flex gap-2",
                input {
                    class: "flex-1 min-w-0 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs font-mono text-[var(--text)] outline-none transition-colors",
                    placeholder: crate::i18n::text("Curator npub1…", &[]),
                    value: "{input}",
                    oninput: move |e| input.set(e.value()),
                }
                button {
                    class: "rounded px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                    onclick: follow,
                    {crate::i18n::text("Follow", &[])}
                }
            }
            if let Some(err) = error() {
                div { class: "text-[11px] text-[var(--danger)]", {crate::i18n::message(&(err).to_string())} }
            }
        }
    }
}
