//! The Agents panel (#453): make an agent, try it, run it on this device,
//! and add it to a guild through the same consent every app gets.

use std::collections::HashSet;

use dioxus::prelude::*;
use discordia_agent::{ApiKey, Harness, Http, Prompt, Role, Status, Turn};
use discordia_bot::BotIdentity;
use zeroize::Zeroizing;

use crate::agents::service::RunKey;
use crate::agents::{AgentEntry, Agents, Preset, RunState, Secrets, ServerRef, store};
use crate::protocol::{AppManifest, Id};
use crate::state::{SessionMode, use_app_state};

const INPUT: &str = "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors";
const LABEL: &str =
    "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1";
const BUTTON: &str = "rounded px-2.5 py-1 text-[11px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors disabled:opacity-50";

fn preset_label(p: Preset) -> String {
    match p {
        Preset::Anthropic => "Anthropic".into(),
        Preset::OpenAi => "OpenAI".into(),
        Preset::OpenRouter => "OpenRouter".into(),
        Preset::Groq => "Groq".into(),
        Preset::Ollama => crate::i18n::text("Ollama (this computer)", &[]),
        Preset::Custom => crate::i18n::text("Other (OpenAI-compatible)", &[]),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Light {
    Off,
    Busy,
    On,
    Failed,
}

fn light_of(runs: &std::collections::HashMap<RunKey, RunState>, agent: &str) -> Light {
    let mut best = Light::Off;
    for (key, run) in runs {
        if key.0 != agent {
            continue;
        }
        let light = match run {
            RunState::Agent(Status::Online) => Light::On,
            RunState::Agent(_) => Light::Busy,
            RunState::Stopped(_) => Light::Failed,
        };
        best = match (best, light) {
            (Light::On, _) | (_, Light::On) => Light::On,
            (Light::Busy, _) | (_, Light::Busy) => Light::Busy,
            (Light::Failed, _) | (_, Light::Failed) => Light::Failed,
            _ => Light::Off,
        };
    }
    best
}

fn dot(light: Light) -> &'static str {
    match light {
        Light::Off => "background-color: var(--text-dim);",
        Light::Busy => "background-color: var(--warn);",
        Light::On => "background-color: var(--success, #3ba55d);",
        Light::Failed => "background-color: var(--danger);",
    }
}

fn describe(run: Option<&RunState>) -> String {
    match run {
        None => crate::i18n::text("Not running", &[]),
        Some(RunState::Agent(Status::Connecting)) => crate::i18n::text("Connecting…", &[]),
        Some(RunState::Agent(Status::Online)) => crate::i18n::text("Online", &[]),
        Some(RunState::Agent(Status::Retrying { error, in_secs })) => crate::i18n::text(
            "Retrying in {0}s: {1}",
            &[in_secs.to_string(), error.clone()],
        ),
        Some(RunState::Stopped(why)) => why.clone(),
    }
}

/// Mounted once at the account root, so it opens over Home or a server.
#[component]
pub fn AgentsDialog() -> Element {
    let mut state = use_app_state();
    let agents = use_context::<Agents>();
    let mut selected = use_signal(|| None::<String>);
    if !state.read().agents_open {
        return rsx! {};
    }
    let list = agents.list.read().clone();
    let runs = agents.runs.read().clone();
    let chosen: Option<(AgentEntry, bool)> = match selected() {
        Some(k) if k.is_empty() => Some((AgentEntry::draft(), true)),
        Some(k) => list
            .iter()
            .find(|e| e.pubkey == k)
            .cloned()
            .map(|e| (e, false)),
        None => None,
    };
    let close = move |_| state.write().agents_open = false;
    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-[60] flex items-center justify-center bg-black/50",
            onclick: close,
            div {
                class: "dxf-modal-in w-[46rem] max-w-[95vw] h-[80vh] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                    h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", {crate::i18n::text("Agents", &[])} }
                    button {
                        class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                        aria_label: crate::i18n::text("Close", &[]),
                        onclick: close,
                        "✕"
                    }
                }
                div { class: "flex-1 min-h-0 flex",
                    div { class: "w-52 shrink-0 border-r border-[var(--border)] flex flex-col",
                        div { class: "flex-1 overflow-y-auto p-2 space-y-1",
                            for e in list.iter() {
                                button {
                                    key: "{e.pubkey}",
                                    class: if selected().as_deref() == Some(e.pubkey.as_str()) {
                                        "w-full text-left rounded px-2 py-1.5 bg-[var(--accent-soft)]"
                                    } else {
                                        "w-full text-left rounded px-2 py-1.5 hover:bg-[var(--hover)]"
                                    },
                                    onclick: {
                                        let k = e.pubkey.clone();
                                        move |_| selected.set(Some(k.clone()))
                                    },
                                    div { class: "flex items-center gap-2",
                                        span { class: "w-2 h-2 rounded-full shrink-0", style: dot(light_of(&runs, &e.pubkey)) }
                                        span { class: "text-xs text-[var(--text)] truncate", "{e.name}" }
                                    }
                                    div { class: "text-[10px] text-[var(--text-dim)] truncate pl-4", "{e.model}" }
                                }
                            }
                        }
                        div { class: "p-2 border-t border-[var(--border)]",
                            button {
                                class: "{BUTTON} w-full",
                                onclick: move |_| selected.set(Some(String::new())),
                                {crate::i18n::text("New agent", &[])}
                            }
                        }
                    }
                    div { class: "flex-1 min-w-0 overflow-y-auto p-4",
                        if let Some((entry, is_new)) = chosen {
                            for (entry, is_new) in std::iter::once((entry, is_new)) {
                                AgentEditor {
                                    key: "{entry.pubkey}-{is_new}",
                                    entry,
                                    is_new,
                                    on_saved: move |k: String| selected.set(Some(k)),
                                    on_deleted: move |_| selected.set(None),
                                }
                            }
                        } else {
                            div { class: "text-xs text-[var(--text-dim)] space-y-2",
                                p { {crate::i18n::text("An agent is an app whose replies come from an AI model you choose. It answers when someone mentions it, replies to it, or uses /ask.", &[])} }
                                p { {crate::i18n::text("It runs on this device while Discordia is open, with your API key, which never leaves this device. A guild admin adds it from that guild's Integrations.", &[])} }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The typed key, or else the one already sealed for this agent.
async fn api_key_for(owner: String, agent: String, typed: String) -> Result<String, String> {
    if !typed.trim().is_empty() || agent.is_empty() {
        return Ok(typed.trim().to_string());
    }
    tokio::task::spawn_blocking(move || store::unseal(&owner, &agent))
        .await
        .map_err(|e| e.to_string())?
        .map(|s| s.api_key.to_string())
}

#[component]
fn AgentEditor(
    entry: AgentEntry,
    is_new: bool,
    on_saved: EventHandler<String>,
    on_deleted: EventHandler<()>,
) -> Element {
    let agents = use_context::<Agents>();
    let identity = use_context::<crate::identity::Identity>();
    let mut form = use_signal(|| entry.clone());
    let mut api_key = use_signal(String::new);
    let mut models = use_signal(Vec::<String>::new);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut test_input = use_signal(String::new);
    let mut test_reply = use_signal(|| None::<String>);
    let mut confirm_delete = use_signal(|| false);

    let f = form.read().clone();
    let runs = agents.runs.read().clone();
    let saved = agents
        .list
        .read()
        .iter()
        .find(|e| e.pubkey == f.pubkey)
        .cloned();

    let load_models = {
        let owner = agents.owner.clone();
        move |_| {
            let owner = owner.clone();
            spawn(async move {
                busy.set(true);
                error.set(None);
                let draft = form.peek().clone();
                let result =
                    match api_key_for(owner, draft.pubkey.clone(), api_key.peek().clone()).await {
                        Ok(key) => Http::new(draft.model_config(ApiKey::new(key)))
                            .models()
                            .await
                            .map_err(|e| e.to_string()),
                        Err(e) => Err(e),
                    };
                match result {
                    Ok(list) if list.is_empty() => error.set(Some(crate::i18n::text(
                        "The provider listed no models.",
                        &[],
                    ))),
                    Ok(list) => models.set(list),
                    Err(e) => error.set(Some(e)),
                }
                busy.set(false);
            });
        }
    };

    let run_test = {
        let owner = agents.owner.clone();
        let me = identity.display_name.clone();
        move |_| {
            let (owner, me) = (owner.clone(), me.clone());
            let question = test_input.peek().trim().to_string();
            if question.is_empty() {
                return;
            }
            spawn(async move {
                busy.set(true);
                error.set(None);
                test_reply.set(None);
                let draft = form.peek().clone();
                let result =
                    match api_key_for(owner, draft.pubkey.clone(), api_key.peek().clone()).await {
                        Ok(key) => {
                            let config = draft.config(ApiKey::new(key));
                            let prompt = Prompt {
                                system: discordia_agent::system_prompt(&config),
                                turns: vec![Turn {
                                    role: Role::User,
                                    text: format!("{me}: {question}"),
                                }],
                            };
                            Http::new(config.model.clone())
                                .reply(prompt)
                                .await
                                .map(|r| r.text)
                                .map_err(|e| e.to_string())
                        }
                        Err(e) => Err(e),
                    };
                match result {
                    Ok(text) => test_reply.set(Some(text)),
                    Err(e) => error.set(Some(e)),
                }
                busy.set(false);
            });
        }
    };

    let save = {
        let agents = agents.clone();
        move |_| {
            let agents = agents.clone();
            spawn(async move {
                let mut next = form.peek().clone();
                let typed = api_key.peek().trim().to_string();
                if let Some(problem) = next.problem(!typed.is_empty() || !is_new) {
                    error.set(Some(problem));
                    return;
                }
                busy.set(true);
                error.set(None);
                let owner = agents.owner.clone();
                let sealed = if is_new {
                    let id = BotIdentity::generate();
                    next.pubkey = id.pubkey().to_string();
                    let secrets = Secrets {
                        identity: Zeroizing::new(id.secret_base58()),
                        api_key: Zeroizing::new(typed),
                    };
                    let (owner, agent) = (owner.clone(), next.pubkey.clone());
                    tokio::task::spawn_blocking(move || store::seal(&owner, &agent, &secrets)).await
                } else if !typed.is_empty() {
                    let (owner, agent) = (owner.clone(), next.pubkey.clone());
                    tokio::task::spawn_blocking(move || {
                        let old = store::unseal(&owner, &agent)?;
                        let secrets = Secrets {
                            identity: old.identity,
                            api_key: Zeroizing::new(typed),
                        };
                        store::seal(&owner, &agent, &secrets)
                    })
                    .await
                } else {
                    Ok(Ok(()))
                };
                match sealed {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        error.set(Some(e));
                        busy.set(false);
                        return;
                    }
                    Err(e) => {
                        error.set(Some(e.to_string()));
                        busy.set(false);
                        return;
                    }
                }
                let mut list = agents.list.peek().clone();
                let previous = list.iter().find(|e| e.pubkey == next.pubkey).cloned();
                next.manifest_version = next.next_version(previous.as_ref());
                match list.iter_mut().find(|e| e.pubkey == next.pubkey) {
                    Some(slot) => *slot = next.clone(),
                    None => list.push(next.clone()),
                }
                if let Err(e) = agents.save(list) {
                    error.set(Some(e));
                } else {
                    if !is_new && !api_key.peek().is_empty() {
                        agents.rekeyed(&next.pubkey);
                    }
                    api_key.set(String::new());
                    form.set(next.clone());
                    on_saved.call(next.pubkey);
                }
                busy.set(false);
            });
        }
    };

    let delete = {
        let agents = agents.clone();
        move |_| {
            let pubkey = form.peek().pubkey.clone();
            let mut list = agents.list.peek().clone();
            list.retain(|e| e.pubkey != pubkey);
            if let Err(e) = agents.save(list) {
                error.set(Some(e));
                return;
            }
            let owner = agents.owner.clone();
            spawn(async move {
                let _ = tokio::task::spawn_blocking(move || store::forget(&owner, &pubkey)).await;
            });
            on_deleted.call(());
        }
    };

    let set_enabled = {
        let agents = agents.clone();
        move |enabled: bool| {
            let pubkey = form.peek().pubkey.clone();
            let mut list = agents.list.peek().clone();
            if let Some(e) = list.iter_mut().find(|e| e.pubkey == pubkey) {
                e.enabled = enabled;
            }
            form.write().enabled = enabled;
            if let Err(e) = agents.save(list) {
                error.set(Some(e));
            }
        }
    };

    let remove_server = {
        let agents = agents.clone();
        move |server: ServerRef| {
            let pubkey = form.peek().pubkey.clone();
            let mut list = agents.list.peek().clone();
            if let Some(e) = list.iter_mut().find(|e| e.pubkey == pubkey) {
                e.servers.retain(|s| *s != server);
            }
            form.write().servers.retain(|s| *s != server);
            if let Err(e) = agents.save(list) {
                error.set(Some(e));
            }
        }
    };

    let key_placeholder = if is_new || !f.preset.needs_key() {
        crate::i18n::text("API key", &[])
    } else {
        crate::i18n::text("Saved on this device. Type to replace it.", &[])
    };
    let disclosed = f.preset.disclosed(&f.base_url);

    rsx! {
        div { class: "space-y-3",
            div {
                div { class: LABEL, {crate::i18n::text("Name", &[])} }
                input {
                    class: INPUT,
                    maxlength: "32",
                    value: "{f.name}",
                    oninput: move |e| form.write().name = e.value(),
                }
            }
            div {
                div { class: LABEL, {crate::i18n::text("Instructions", &[])} }
                textarea {
                    class: "{INPUT} h-24 resize-y",
                    placeholder: crate::i18n::text("Who it is and how it should answer.", &[]),
                    value: "{f.prompt}",
                    oninput: move |e| form.write().prompt = e.value(),
                }
            }
            div { class: "grid grid-cols-2 gap-3",
                div {
                    div { class: LABEL, {crate::i18n::text("Provider", &[])} }
                    select {
                        class: INPUT,
                        onchange: move |e| {
                            if let Some(p) = Preset::from_id(&e.value()) {
                                let mut w = form.write();
                                if w.preset != p {
                                    w.preset = p;
                                    w.base_url = p.base_url().into();
                                    w.model = p.default_model().into();
                                    models.set(Vec::new());
                                }
                            }
                        },
                        for p in Preset::ALL {
                            option { key: "{p.id()}", value: p.id(), selected: p == f.preset, {preset_label(p)} }
                        }
                    }
                }
                div {
                    div { class: LABEL, {crate::i18n::text("Model", &[])} }
                    div { class: "flex gap-1.5",
                        input {
                            class: "{INPUT} font-mono",
                            list: "dxf-agent-models",
                            value: "{f.model}",
                            oninput: move |e| form.write().model = e.value(),
                        }
                        button { class: BUTTON, disabled: busy(), onclick: load_models, {crate::i18n::text("List", &[])} }
                    }
                    datalist { id: "dxf-agent-models",
                        for m in models.read().iter() {
                            option { key: "{m}", value: "{m}" }
                        }
                    }
                }
            }
            if f.preset.editable_address() {
                div {
                    div { class: LABEL, {crate::i18n::text("Address", &[])} }
                    input {
                        class: "{INPUT} font-mono",
                        placeholder: "http://127.0.0.1:11434/v1",
                        value: "{f.base_url}",
                        oninput: move |e| form.write().base_url = e.value(),
                    }
                }
            }
            div {
                div { class: LABEL, {crate::i18n::text("API key", &[])} }
                input {
                    class: "{INPUT} font-mono",
                    r#type: "password",
                    autocomplete: "off",
                    placeholder: "{key_placeholder}",
                    value: "{api_key}",
                    oninput: move |e| api_key.set(e.value()),
                }
            }
            div { class: "grid grid-cols-2 gap-3",
                div {
                    div { class: LABEL, {crate::i18n::text("Questions per person per hour", &[])} }
                    input {
                        class: INPUT,
                        r#type: "number",
                        min: "1",
                        value: "{f.per_member_per_hour}",
                        oninput: move |e| if let Ok(n) = e.value().parse::<u32>() { form.write().per_member_per_hour = n.max(1) },
                    }
                }
                div {
                    div { class: LABEL, {crate::i18n::text("Tokens per day", &[])} }
                    input {
                        class: INPUT,
                        r#type: "number",
                        min: "1000",
                        step: "1000",
                        value: "{f.daily_tokens}",
                        oninput: move |e| if let Ok(n) = e.value().parse::<u64>() { form.write().daily_tokens = n.max(1000) },
                    }
                }
            }
            div { class: "text-[10px] text-[var(--warn)]",
                {crate::i18n::text("Members are told that what it reads is sent to {0}.", &[disclosed])}
            }
            if let Some(e) = error() {
                div { class: "text-[11px] text-[var(--danger)] break-words", "{e}" }
            }
            div { class: "flex items-center gap-2",
                button { class: BUTTON, disabled: busy(), onclick: save,
                    if is_new { {crate::i18n::text("Create", &[])} } else { {crate::i18n::text("Save", &[])} }
                }
                if !is_new {
                    if saved.as_ref().is_some_and(|s| s.enabled) {
                        button { class: BUTTON, onclick: { let mut set = set_enabled.clone(); move |_| set(false) }, {crate::i18n::text("Pause", &[])} }
                    } else {
                        button { class: BUTTON, onclick: { let mut set = set_enabled.clone(); move |_| set(true) }, {crate::i18n::text("Resume", &[])} }
                    }
                    div { class: "flex-1" }
                    if confirm_delete() {
                        button { class: "{BUTTON} text-[var(--danger)]", onclick: delete, {crate::i18n::text("Delete for good", &[])} }
                    } else {
                        button { class: BUTTON, onclick: move |_| confirm_delete.set(true), {crate::i18n::text("Delete", &[])} }
                    }
                }
            }
            if confirm_delete() {
                div { class: "text-[10px] text-[var(--text-dim)]",
                    {crate::i18n::text("Its keys are erased from this device. Guilds that installed it keep it until an admin uninstalls it.", &[])}
                }
            }

            div { class: "pt-2 border-t border-[var(--border)]",
                div { class: LABEL, {crate::i18n::text("Try it", &[])} }
                div { class: "flex gap-1.5",
                    input {
                        class: INPUT,
                        placeholder: crate::i18n::text("Ask something, as if you mentioned it", &[]),
                        value: "{test_input}",
                        oninput: move |e| test_input.set(e.value()),
                        onkeydown: { let run = run_test.clone(); move |e: KeyboardEvent| if e.key() == Key::Enter { run(()) } },
                    }
                    button { class: BUTTON, disabled: busy(), onclick: { let run = run_test.clone(); move |_| run(()) }, {crate::i18n::text("Send", &[])} }
                }
                if let Some(reply) = test_reply() {
                    div { class: "mt-2 rounded border border-[var(--border)] p-2 text-xs text-[var(--text)] whitespace-pre-wrap", "{reply}" }
                }
            }

            if !is_new {
                div { class: "pt-2 border-t border-[var(--border)]",
                    div { class: LABEL, {crate::i18n::text("Where it runs", &[])} }
                    if f.servers.is_empty() {
                        div { class: "text-[11px] text-[var(--text-dim)]",
                            {crate::i18n::text("Nowhere yet. Open a guild's Integrations to add it there.", &[])}
                        }
                    }
                    for server in f.servers.iter().cloned() {
                        div { key: "{server.label()}", class: "flex items-center gap-2 py-1",
                            span {
                                class: "w-2 h-2 rounded-full shrink-0",
                                style: dot(match runs.get(&(f.pubkey.clone(), server.clone())) {
                                    Some(RunState::Agent(Status::Online)) => Light::On,
                                    Some(RunState::Agent(_)) => Light::Busy,
                                    Some(RunState::Stopped(_)) => Light::Failed,
                                    None => Light::Off,
                                }),
                            }
                            div { class: "flex-1 min-w-0",
                                div { class: "text-xs text-[var(--text)] truncate", {server.label()} }
                                div { class: "text-[10px] text-[var(--text-dim)] truncate",
                                    {describe(runs.get(&(f.pubkey.clone(), server.clone())))}
                                }
                            }
                            button {
                                class: "text-[10px] text-[var(--text-dim)] hover:text-[var(--danger)]",
                                onclick: { let mut remove = remove_server.clone(); let s = server.clone(); move |_| remove(s.clone()) },
                                {crate::i18n::text("Stop here", &[])}
                            }
                        }
                    }
                }
            }
        }
    }
}

/// In a guild's Integrations: one's own agents, installed through the same consent as any app.
#[component]
pub fn MyAgentsSection(guild_id: Id) -> Element {
    let mut state = use_app_state();
    let agents = use_context::<Agents>();
    let mode = try_use_context::<SessionMode>();
    let mut consent = use_signal(|| None::<(String, AppManifest, String)>);
    let mut error = use_signal(|| None::<String>);

    let here = crate::agents::current_server(mode.as_ref(), &state.read());
    let installed: HashSet<String> = state
        .read()
        .integrations
        .get(&guild_id)
        .map(|bots| bots.iter().map(|b| b.bot_pubkey.clone()).collect())
        .unwrap_or_default();
    let list = agents.list.read().clone();

    rsx! {
        div {
            div { class: "flex items-center mb-1.5",
                div { class: "flex-1 text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]",
                    {crate::i18n::text("Your agents", &[])}
                }
                button {
                    class: "text-[10px] text-[var(--accent)] hover:underline",
                    onclick: move |_| state.write().agents_open = true,
                    {crate::i18n::text("Manage", &[])}
                }
            }
            if list.is_empty() {
                div { class: "text-xs text-[var(--text-dim)] py-1",
                    {crate::i18n::text("You have no agents yet.", &[])}
                }
            }
            for e in list.iter().cloned() {
                div { key: "{e.pubkey}", class: "flex items-center gap-2 py-1",
                    div { class: "flex-1 min-w-0",
                        div { class: "text-xs text-[var(--text)] truncate", "{e.name}" }
                        div { class: "text-[10px] text-[var(--text-dim)] truncate", "{e.model}" }
                    }
                    if installed.contains(&e.pubkey) {
                        span { class: "text-[10px] text-[var(--text-dim)]", {crate::i18n::text("Installed", &[])} }
                    } else {
                        button {
                            class: BUTTON,
                            disabled: here.is_none(),
                            onclick: {
                                let owner = agents.owner.clone();
                                move |_| {
                                    let (owner, entry) = (owner.clone(), e.clone());
                                    spawn(async move {
                                        error.set(None);
                                        let agent = entry.pubkey.clone();
                                        let signed = tokio::task::spawn_blocking(move || {
                                            let keys = store::unseal(&owner, &agent)?;
                                            let id = BotIdentity::from_base58_secret(&keys.identity).map_err(|e| e.to_string())?;
                                            let manifest = entry.config(ApiKey::new("")).manifest();
                                            let event = id.sign_manifest(&manifest);
                                            Ok::<_, String>((entry.pubkey.clone(), manifest, event))
                                        })
                                        .await
                                        .map_err(|e| e.to_string())
                                        .and_then(|r| r);
                                        match signed {
                                            Ok(c) => consent.set(Some(c)),
                                            Err(e) => error.set(Some(e)),
                                        }
                                    });
                                }
                            },
                            {crate::i18n::text("Add here", &[])}
                        }
                    }
                }
            }
            if let Some(e) = error() {
                div { class: "text-[11px] text-[var(--danger)]", "{e}" }
            }
            if let Some((bot_pubkey, manifest, event)) = consent() {
                super::integrations::AppConsent {
                    guild_id,
                    bot_pubkey: bot_pubkey.clone(),
                    manifest,
                    event,
                    on_done: {
                        let agents = agents.clone();
                        let here = here.clone();
                        move |_| {
                            consent.set(None);
                            let Some(here) = here.clone() else { return };
                            let mut list = agents.list.peek().clone();
                            if let Some(entry) = list.iter_mut().find(|e| e.pubkey == bot_pubkey)
                                && !entry.servers.contains(&here)
                            {
                                entry.servers.push(here);
                            }
                            if let Err(e) = agents.save(list) {
                                error.set(Some(e));
                            }
                        }
                    },
                }
            }
        }
    }
}
