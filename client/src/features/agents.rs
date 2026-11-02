//! The Agents panel (#453): make an agent, try it, run it on this device,
//! and add it to a guild through the same consent every app gets.

use std::collections::HashSet;

use dioxus::prelude::*;
use discordia_agent::{ApiKey, Harness, Http, Prompt, Role, Status, Turn};
use discordia_bot::BotIdentity;
use zeroize::Zeroizing;

use crate::agents::service::RunKey;
use crate::agents::{AgentEntry, Agents, Preset, RunState, Secrets, ServerRef, store};
use crate::protocol::{
    AppManifest, AppsPolicy, ClientMessage, Id, Permission, ServerAgentSpec, ServerAgentView,
};
use crate::state::{ConnectionStatus, SessionMode, use_app_state, use_gateway};

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

#[derive(Clone, PartialEq)]
enum Pick {
    /// A personal agent by key; empty for a new one.
    Device(String),
    /// A server agent by key; empty for a new one.
    Server(String),
    /// The operator's rules for every guild's apps.
    Apps,
}

/// Mounted at Home's root and a server's: the server half speaks to that server's gateway.
#[component]
pub fn AgentsDialog() -> Element {
    let mut state = use_app_state();
    let agents = use_context::<Agents>();
    let gateway = use_gateway();
    let mut selected = use_signal(|| None::<Pick>);
    let open = use_memo(move || state.read().agents_open);
    let operator = use_memo(move || {
        let s = state.read();
        s.is_operator && s.status == ConnectionStatus::Ready
    });
    use_effect(move || {
        if open() && operator() {
            gateway.send(ClientMessage::FetchServerAgents);
        }
    });
    if !open() {
        return rsx! {};
    }
    let list = agents.list.read().clone();
    let runs = agents.runs.read().clone();
    let on_server = if operator() {
        state.read().server_agents.clone()
    } else {
        Vec::new()
    };
    let close = move |_| state.write().agents_open = false;
    let item_class = move |pick: Pick| {
        if selected().as_ref() == Some(&pick) {
            "w-full text-left rounded px-2 py-1.5 bg-[var(--accent-soft)]"
        } else {
            "w-full text-left rounded px-2 py-1.5 hover:bg-[var(--hover)]"
        }
    };
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
                            if operator() {
                                div { class: "{LABEL} px-2 pt-1", {crate::i18n::text("On this device", &[])} }
                            }
                            for e in list.iter() {
                                button {
                                    key: "d-{e.pubkey}",
                                    class: item_class(Pick::Device(e.pubkey.clone())),
                                    onclick: {
                                        let k = e.pubkey.clone();
                                        move |_| selected.set(Some(Pick::Device(k.clone())))
                                    },
                                    div { class: "flex items-center gap-2",
                                        span { class: "w-2 h-2 rounded-full shrink-0", style: dot(light_of(&runs, &e.pubkey)) }
                                        span { class: "text-xs text-[var(--text)] truncate", "{e.name}" }
                                    }
                                    div { class: "text-[10px] text-[var(--text-dim)] truncate pl-4", "{e.model}" }
                                }
                            }
                            if operator() {
                                div { class: "{LABEL} px-2 pt-3", {crate::i18n::text("On this server", &[])} }
                                for v in on_server.iter() {
                                    button {
                                        key: "s-{v.pubkey}",
                                        class: item_class(Pick::Server(v.pubkey.clone())),
                                        onclick: {
                                            let k = v.pubkey.clone();
                                            move |_| selected.set(Some(Pick::Server(k.clone())))
                                        },
                                        div { class: "flex items-center gap-2",
                                            span {
                                                class: "w-2 h-2 rounded-full shrink-0",
                                                style: dot(if v.online { Light::On } else { Light::Busy }),
                                            }
                                            span { class: "text-xs text-[var(--text)] truncate", "{v.spec.name}" }
                                        }
                                        div { class: "text-[10px] text-[var(--text-dim)] truncate pl-4", "{v.spec.model}" }
                                    }
                                }
                            }
                        }
                        div { class: "p-2 border-t border-[var(--border)] space-y-1.5",
                            button {
                                class: "{BUTTON} w-full",
                                onclick: move |_| selected.set(Some(Pick::Device(String::new()))),
                                {crate::i18n::text("New agent", &[])}
                            }
                            if operator() {
                                button {
                                    class: "{BUTTON} w-full",
                                    onclick: move |_| selected.set(Some(Pick::Server(String::new()))),
                                    {crate::i18n::text("New server agent", &[])}
                                }
                                button {
                                    class: "{BUTTON} w-full",
                                    onclick: move |_| selected.set(Some(Pick::Apps)),
                                    {crate::i18n::text("Apps on this server", &[])}
                                }
                            }
                        }
                    }
                    div { class: "flex-1 min-w-0 overflow-y-auto p-4",
                        match selected() {
                            Some(Pick::Device(k)) => {
                                let chosen = if k.is_empty() {
                                    Some((AgentEntry::draft(), true))
                                } else {
                                    list.iter().find(|e| e.pubkey == k).cloned().map(|e| (e, false))
                                };
                                rsx! {
                                    for (entry, is_new) in chosen.into_iter() {
                                        AgentEditor {
                                            key: "{entry.pubkey}-{is_new}",
                                            entry,
                                            is_new,
                                            on_saved: move |k: String| selected.set(Some(Pick::Device(k))),
                                            on_deleted: move |_| selected.set(None),
                                        }
                                    }
                                }
                            }
                            Some(Pick::Server(k)) => {
                                let view = on_server.iter().find(|v| v.pubkey == k).cloned();
                                let known = k.is_empty() || view.is_some();
                                rsx! {
                                    for view in known.then_some(view).into_iter() {
                                        ServerAgentEditor {
                                            key: "server-{k}",
                                            view,
                                            on_done: move |_| selected.set(None),
                                        }
                                    }
                                }
                            }
                            Some(Pick::Apps) => rsx! { ServerAppsPanel {} },
                            None => rsx! {
                                div { class: "text-xs text-[var(--text-dim)] space-y-2",
                                    p { {crate::i18n::text("An agent is an app whose replies come from an AI model you choose. It answers when someone mentions it, replies to it, or uses /ask.", &[])} }
                                    p { {crate::i18n::text("It runs on this device while Discordia is open, with your API key, which never leaves this device. A guild admin adds it from that guild's Integrations.", &[])} }
                                    if operator() {
                                        p { {crate::i18n::text("As this server's operator you can also run agents inside the server, so they stay online when your app is closed.", &[])} }
                                    }
                                }
                            },
                        }
                    }
                }
            }
        }
    }
}

/// The typed key, or else the one sealed on this device for this agent.
/// `owner` is `None` for a server agent, whose key this device never sees.
async fn api_key_for(
    owner: Option<String>,
    agent: String,
    typed: String,
    needs_key: bool,
) -> Result<String, String> {
    let typed = typed.trim().to_string();
    match owner {
        Some(owner) if typed.is_empty() && !agent.is_empty() => {
            tokio::task::spawn_blocking(move || store::unseal(&owner, &agent))
                .await
                .map_err(|e| e.to_string())?
                .map(|s| s.api_key.to_string())
        }
        None if typed.is_empty() && needs_key => Err(crate::i18n::text(
            "Type the API key here to do this from this device.",
            &[],
        )),
        _ => Ok(typed),
    }
}

/// What both kinds of agent are made of; the caller owns saving.
#[component]
fn AgentFields(
    form: Signal<AgentEntry>,
    api_key: Signal<String>,
    busy: Signal<bool>,
    error: Signal<Option<String>>,
    key_placeholder: String,
    owner: Option<String>,
) -> Element {
    let mut form = form;
    let mut api_key = api_key;
    let mut busy = busy;
    let mut error = error;
    let mut models = use_signal(Vec::<String>::new);
    let f = form.read().clone();
    let load_models = move |_| {
        let owner = owner.clone();
        spawn(async move {
            busy.set(true);
            error.set(None);
            let draft = form.peek().clone();
            let key = api_key_for(
                owner,
                draft.pubkey.clone(),
                api_key.peek().clone(),
                draft.preset.needs_key(),
            )
            .await;
            let result = match key {
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
    };
    let disclosed = f.preset.disclosed(&f.base_url);
    rsx! {
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
    }
}

/// Asks the model straight from this device, with the form as it stands.
#[component]
fn TryIt(
    form: Signal<AgentEntry>,
    api_key: Signal<String>,
    busy: Signal<bool>,
    error: Signal<Option<String>>,
    owner: Option<String>,
) -> Element {
    let mut busy = busy;
    let mut error = error;
    let identity = use_context::<crate::identity::Identity>();
    let mut input = use_signal(String::new);
    let mut reply = use_signal(|| None::<String>);
    let run = move |_| {
        let (owner, me) = (owner.clone(), identity.display_name.clone());
        let question = input.peek().trim().to_string();
        if question.is_empty() {
            return;
        }
        spawn(async move {
            busy.set(true);
            error.set(None);
            reply.set(None);
            let draft = form.peek().clone();
            let key = api_key_for(
                owner,
                draft.pubkey.clone(),
                api_key.peek().clone(),
                draft.preset.needs_key(),
            )
            .await;
            let result = match key {
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
                Ok(text) => reply.set(Some(text)),
                Err(e) => error.set(Some(e)),
            }
            busy.set(false);
        });
    };
    rsx! {
        div { class: "pt-2 border-t border-[var(--border)]",
            div { class: LABEL, {crate::i18n::text("Try it", &[])} }
            div { class: "flex gap-1.5",
                input {
                    class: INPUT,
                    placeholder: crate::i18n::text("Ask something, as if you mentioned it", &[]),
                    value: "{input}",
                    oninput: move |e| input.set(e.value()),
                    onkeydown: { let run = run.clone(); move |e: KeyboardEvent| if e.key() == Key::Enter { run(()) } },
                }
                button { class: BUTTON, disabled: busy(), onclick: { let run = run.clone(); move |_| run(()) }, {crate::i18n::text("Send", &[])} }
            }
            if let Some(text) = reply() {
                div { class: "mt-2 rounded border border-[var(--border)] p-2 text-xs text-[var(--text)] whitespace-pre-wrap", "{text}" }
            }
        }
    }
}

#[component]
fn AgentEditor(
    entry: AgentEntry,
    is_new: bool,
    on_saved: EventHandler<String>,
    on_deleted: EventHandler<()>,
) -> Element {
    let agents = use_context::<Agents>();
    let mut form = use_signal(|| entry.clone());
    let mut api_key = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut confirm_delete = use_signal(|| false);
    let mut dm_raw = use_signal(|| entry.dm_allow.join("\n"));
    let nostr = try_use_context::<crate::nostr::service::NostrTx>();
    let mut state = use_app_state();

    let f = form.read().clone();
    let runs = agents.runs.read().clone();
    let saved = agents
        .list
        .read()
        .iter()
        .find(|e| e.pubkey == f.pubkey)
        .cloned();

    let save = {
        let agents = agents.clone();
        move |_| {
            let agents = agents.clone();
            spawn(async move {
                let mut next = form.peek().clone();
                next.dm_allow = dm_raw
                    .peek()
                    .lines()
                    .filter_map(|l| crate::identity::pubkey_from_input(l.trim()).ok())
                    .collect();
                let typed = api_key.peek().trim().to_string();
                if let Some(problem) = next.problem(!typed.is_empty() || !is_new) {
                    error.set(Some(problem));
                    return;
                }
                busy.set(true);
                error.set(None);
                let owner = agents.owner.clone();
                let rekeyed = !is_new && !typed.is_empty();
                let sealed = if is_new {
                    let id = BotIdentity::generate();
                    next.pubkey = id.pubkey().to_string();
                    let secrets = Secrets {
                        identity: Zeroizing::new(id.secret_base58()),
                        api_key: Zeroizing::new(typed),
                    };
                    let (owner, agent) = (owner.clone(), next.pubkey.clone());
                    tokio::task::spawn_blocking(move || store::seal(&owner, &agent, &secrets)).await
                } else if rekeyed {
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
                    if rekeyed {
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

    rsx! {
        div { class: "space-y-3",
            AgentFields { form, api_key, busy, error, key_placeholder, owner: Some(agents.owner.clone()) }
            div { class: "space-y-1.5",
                label { class: "flex items-center gap-2 text-xs text-[var(--text)]",
                    input {
                        r#type: "checkbox",
                        checked: f.answer_dms,
                        onchange: move |e| form.write().answer_dms = e.checked(),
                    }
                    {crate::i18n::text("Answer my direct messages", &[])}
                }
                if f.answer_dms {
                    div {
                        div { class: LABEL, {crate::i18n::text("Others it answers, one key per line", &[])} }
                        textarea {
                            class: "{INPUT} h-14 resize-y font-mono",
                            placeholder: "npub1…",
                            value: "{dm_raw}",
                            oninput: move |e| dm_raw.set(e.value()),
                        }
                        div { class: "text-[10px] text-[var(--text-dim)]",
                            {crate::i18n::text("It ignores everyone else. It reads and answers over your DM relays while this app is open.", &[])}
                        }
                    }
                }
            }
            div { class: "flex items-center gap-2",
                button { class: BUTTON, disabled: busy(), onclick: save,
                    if is_new { {crate::i18n::text("Create", &[])} } else { {crate::i18n::text("Save", &[])} }
                }
                if !is_new {
                    if saved.as_ref().is_some_and(|s| s.answer_dms) {
                        button {
                            class: BUTTON,
                            onclick: {
                                let (nostr, peer) = (nostr.clone(), f.pubkey.clone());
                                move |_| {
                                    if let Some(n) = &nostr {
                                        n.send(crate::nostr::service::NostrCmd::Open { peer: peer.clone() });
                                    }
                                    state.write().agents_open = false;
                                }
                            },
                            {crate::i18n::text("Message it", &[])}
                        }
                    }
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
            TryIt { form, api_key, busy, error, owner: Some(agents.owner.clone()) }
            if !is_new {
                div { class: "pt-2 border-t border-[var(--border)]",
                    div { class: LABEL, {crate::i18n::text("Where it runs", &[])} }
                    if f.servers.is_empty() && !f.answer_dms {
                        div { class: "text-[11px] text-[var(--text-dim)]",
                            {crate::i18n::text("Nowhere yet. Open a guild's Integrations to add it there.", &[])}
                        }
                    }
                    if saved.as_ref().is_some_and(|s| s.answer_dms) {
                        div { class: "flex items-center gap-2 py-1",
                            span {
                                class: "w-2 h-2 rounded-full shrink-0",
                                style: dot(match runs.get(&(f.pubkey.clone(), ServerRef::Dms)) {
                                    Some(RunState::Agent(Status::Online)) => Light::On,
                                    Some(RunState::Agent(_)) => Light::Busy,
                                    Some(RunState::Stopped(_)) => Light::Failed,
                                    None => Light::Off,
                                }),
                            }
                            div { class: "flex-1 min-w-0",
                                div { class: "text-xs text-[var(--text)] truncate", {ServerRef::Dms.label()} }
                                div { class: "text-[10px] text-[var(--text-dim)] truncate",
                                    {describe(runs.get(&(f.pubkey.clone(), ServerRef::Dms)))}
                                }
                            }
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

/// An agent the operator runs in the server (#454). Its key goes up once
/// and never comes back; the server answers with `ServerAgents`.
#[component]
fn ServerAgentEditor(view: Option<ServerAgentView>, on_done: EventHandler<()>) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let is_new = view.is_none();
    let form = use_signal(|| {
        view.as_ref()
            .map(AgentEntry::from_server)
            .unwrap_or_else(AgentEntry::draft)
    });
    let mut guilds = use_signal(|| {
        view.as_ref()
            .map(|v| v.spec.guilds.clone())
            .unwrap_or_default()
    });
    let mut offered = use_signal(|| view.as_ref().is_some_and(|v| v.spec.offered));
    let mut guild_tokens = use_signal(|| view.as_ref().map_or(0, |v| v.spec.guild_daily_tokens));
    let mut api_key = use_signal(String::new);
    let busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut confirm_delete = use_signal(|| false);

    let manageable: Vec<(Id, String)> = {
        let s = state.read();
        s.guilds
            .iter()
            .filter(|g| s.can(g.id, Permission::ManageGuild))
            .map(|g| (g.id, g.name.clone()))
            .collect()
    };
    let pubkey = view.as_ref().map(|v| v.pubkey.clone());
    let save = {
        let (gateway, pubkey) = (gateway.clone(), pubkey.clone());
        move |_| {
            let entry = form.peek().clone();
            let typed = api_key.peek().trim().to_string();
            if let Some(problem) = entry.problem(!typed.is_empty() || !is_new) {
                error.set(Some(problem));
                return;
            }
            error.set(None);
            gateway.send(ClientMessage::SaveServerAgent {
                pubkey: pubkey.clone(),
                spec: ServerAgentSpec {
                    offered: *offered.peek(),
                    guild_daily_tokens: *guild_tokens.peek(),
                    ..entry.server_spec(guilds.peek().clone())
                },
                api_key: (!typed.is_empty()).then_some(typed),
            });
            api_key.set(String::new());
            if is_new {
                on_done.call(());
            }
        }
    };
    let delete = {
        let (gateway, pubkey) = (gateway.clone(), pubkey.clone());
        move |_| {
            if let Some(pubkey) = pubkey.clone() {
                gateway.send(ClientMessage::DeleteServerAgent { pubkey });
            }
            on_done.call(());
        }
    };
    let key_placeholder = match view.as_ref().map(|v| v.key_hint.as_str()) {
        Some(hint) if !hint.is_empty() => crate::i18n::text(
            "Saved on the server, ending in {0}. Type to replace it.",
            &[hint.to_string()],
        ),
        _ => crate::i18n::text("API key", &[]),
    };
    let status = view.as_ref().map(|v| {
        if v.online {
            crate::i18n::text("Online", &[])
        } else {
            v.status.clone()
        }
    });

    rsx! {
        div { class: "space-y-3",
            div { class: "text-[11px] text-[var(--text-dim)]",
                {crate::i18n::text("Runs inside this server, so it stays online when your app is closed. The server keeps its key and never sends it back.", &[])}
            }
            if let Some(status) = status {
                div { class: "flex items-center gap-2 text-[11px] text-[var(--text)]",
                    span {
                        class: "w-2 h-2 rounded-full shrink-0",
                        style: dot(if view.as_ref().is_some_and(|v| v.online) { Light::On } else { Light::Busy }),
                    }
                    "{status}"
                }
            }
            AgentFields { form, api_key, busy, error, key_placeholder, owner: None }
            div {
                div { class: LABEL, {crate::i18n::text("Guilds", &[])} }
                if manageable.is_empty() {
                    div { class: "text-[11px] text-[var(--text-dim)]", {crate::i18n::text("Only guilds you manage are listed.", &[])} }
                }
                for (id, name) in manageable.iter().cloned() {
                    label { key: "{id}", class: "flex items-center gap-2 py-0.5 text-xs text-[var(--text)]",
                        input {
                            r#type: "checkbox",
                            checked: guilds.read().contains(&id),
                            onchange: move |e| {
                                let mut g = guilds.write();
                                g.retain(|x| *x != id);
                                if e.checked() {
                                    g.push(id);
                                }
                            },
                        }
                        "{name}"
                    }
                }
            }
            div { class: "space-y-1.5",
                label { class: "flex items-center gap-2 text-xs text-[var(--text)]",
                    input {
                        r#type: "checkbox",
                        checked: offered(),
                        onchange: move |e| offered.set(e.checked()),
                    }
                    {crate::i18n::text("Offer it to other guilds' admins", &[])}
                }
                if offered() {
                    div {
                        div { class: LABEL, {crate::i18n::text("Tokens per guild per day (0 for no limit)", &[])} }
                        input {
                            class: INPUT,
                            r#type: "number",
                            min: "0",
                            step: "1000",
                            value: "{guild_tokens}",
                            oninput: move |e| if let Ok(n) = e.value().parse::<u64>() { guild_tokens.set(n) },
                        }
                    }
                }
            }
            div { class: "flex items-center gap-2",
                button { class: BUTTON, disabled: busy(), onclick: save,
                    if is_new { {crate::i18n::text("Create", &[])} } else { {crate::i18n::text("Save", &[])} }
                }
                if !is_new {
                    div { class: "flex-1" }
                    if confirm_delete() {
                        button { class: "{BUTTON} text-[var(--danger)]", onclick: delete, {crate::i18n::text("Delete for good", &[])} }
                    } else {
                        button { class: BUTTON, onclick: move |_| confirm_delete.set(true), {crate::i18n::text("Delete", &[])} }
                    }
                }
            }
            TryIt { form, api_key, busy, error, owner: None }
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

/// The operator's levers over every guild's apps (#455, trap 39).
#[component]
fn ServerAppsPanel() -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let mut approve_input = use_signal(String::new);
    let mut ban_input = use_signal(String::new);
    {
        let gateway = gateway.clone();
        use_hook(move || gateway.send(ClientMessage::FetchServerApps));
    }
    let Some(apps) = state.read().server_apps.clone() else {
        return rsx! {
            div { class: "text-xs text-[var(--text-dim)]", {crate::i18n::text("Loading…", &[])} }
        };
    };
    let set_policy = {
        let gateway = gateway.clone();
        move |policy: AppsPolicy| gateway.send(ClientMessage::SetAppsPolicy { policy })
    };
    let approve = {
        let gateway = gateway.clone();
        move |pubkey: String, approved: bool| {
            gateway.send(ClientMessage::SetAppApproved { pubkey, approved })
        }
    };
    let ban = {
        let gateway = gateway.clone();
        move |pubkey: String, banned: bool| {
            gateway.send(ClientMessage::SetAppBanned { pubkey, banned })
        }
    };
    let choices = [
        (
            AppsPolicy::Open,
            crate::i18n::text("Guild admins install any app", &[]),
        ),
        (
            AppsPolicy::Approved,
            crate::i18n::text("Only apps I approve", &[]),
        ),
        (AppsPolicy::Off, crate::i18n::text("No apps", &[])),
    ];
    rsx! {
        div { class: "space-y-4",
            div {
                div { class: LABEL, {crate::i18n::text("Who may install apps", &[])} }
                for (policy, label) in choices {
                    label { key: "{label}", class: "flex items-center gap-2 py-0.5 text-xs text-[var(--text)]",
                        input {
                            r#type: "radio",
                            name: "dxf-apps-policy",
                            checked: apps.policy == policy,
                            onchange: { let set = set_policy.clone(); move |_| set(policy) },
                        }
                        "{label}"
                    }
                }
                div { class: "text-[10px] text-[var(--text-dim)] mt-1",
                    {crate::i18n::text("Your own installs and the agents you offer are never blocked. Changing this removes nothing already installed.", &[])}
                }
            }
            div {
                div { class: LABEL, {crate::i18n::text("Approved apps", &[])} }
                for key in apps.approved.iter().cloned() {
                    div { key: "a-{key}", class: "flex items-center gap-2 py-0.5",
                        span { class: "flex-1 font-mono text-[10px] text-[var(--text)] truncate", "{key}" }
                        button {
                            class: "text-[10px] text-[var(--text-dim)] hover:text-[var(--danger)]",
                            onclick: { let approve = approve.clone(); let k = key.clone(); move |_| approve(k.clone(), false) },
                            {crate::i18n::text("Remove", &[])}
                        }
                    }
                }
                div { class: "flex gap-1.5 mt-1",
                    input {
                        class: "{INPUT} font-mono",
                        placeholder: crate::i18n::text("App key (npub or hex)", &[]),
                        value: "{approve_input}",
                        oninput: move |e| approve_input.set(e.value()),
                    }
                    button {
                        class: BUTTON,
                        onclick: { let approve = approve.clone(); move |_| {
                            let raw = approve_input.peek().clone();
                            if let Ok(key) = crate::identity::pubkey_from_input(&raw) {
                                approve(key, true);
                                approve_input.set(String::new());
                            }
                        } },
                        {crate::i18n::text("Approve", &[])}
                    }
                }
            }
            div {
                div { class: LABEL, {crate::i18n::text("Banned apps", &[])} }
                for key in apps.banned.iter().cloned() {
                    div { key: "b-{key}", class: "flex items-center gap-2 py-0.5",
                        span { class: "flex-1 font-mono text-[10px] text-[var(--text)] truncate", "{key}" }
                        button {
                            class: "text-[10px] text-[var(--text-dim)] hover:text-[var(--accent)]",
                            onclick: { let ban = ban.clone(); let k = key.clone(); move |_| ban(k.clone(), false) },
                            {crate::i18n::text("Lift ban", &[])}
                        }
                    }
                }
                div { class: "flex gap-1.5 mt-1",
                    input {
                        class: "{INPUT} font-mono",
                        placeholder: crate::i18n::text("App key (npub or hex)", &[]),
                        value: "{ban_input}",
                        oninput: move |e| ban_input.set(e.value()),
                    }
                    button {
                        class: "{BUTTON} text-[var(--danger)]",
                        onclick: { let ban = ban.clone(); move |_| {
                            let raw = ban_input.peek().clone();
                            if let Ok(key) = crate::identity::pubkey_from_input(&raw) {
                                ban(key, true);
                                ban_input.set(String::new());
                            }
                        } },
                        {crate::i18n::text("Ban", &[])}
                    }
                }
                div { class: "text-[10px] text-[var(--text-dim)] mt-1",
                    {crate::i18n::text("A ban removes the app from every guild on this server and stops it being installed again.", &[])}
                }
            }
            div {
                div { class: LABEL, {crate::i18n::text("Installed in every guild", &[])} }
                if apps.installs.is_empty() {
                    div { class: "text-[11px] text-[var(--text-dim)]", {crate::i18n::text("No guild has installed an app.", &[])} }
                }
                for item in apps.installs.iter().cloned() {
                    div { key: "{item.install.guild_id}-{item.install.bot_pubkey}", class: "flex items-center gap-2 py-0.5",
                        div { class: "flex-1 min-w-0",
                            div { class: "text-xs text-[var(--text)] truncate",
                                "{item.install.name}"
                                if item.install.app.as_ref().is_some_and(|a| a.agent.is_some()) {
                                    span { class: "ml-1.5 text-[9px] font-bold uppercase text-[var(--warn)]", {crate::i18n::text("AI", &[])} }
                                }
                            }
                            div { class: "text-[10px] text-[var(--text-dim)] truncate", "{item.guild_name}" }
                        }
                        button {
                            class: "text-[10px] text-[var(--text-dim)] hover:text-[var(--danger)]",
                            onclick: { let ban = ban.clone(); let k = item.install.bot_pubkey.clone(); move |_| ban(k.clone(), true) },
                            {crate::i18n::text("Ban", &[])}
                        }
                    }
                }
            }
        }
    }
}

/// In a guild's Integrations: server agents the operator offers to every guild.
#[component]
pub fn OfferedAgentsSection(guild_id: Id) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let mut looked_up = use_signal(|| None::<String>);
    {
        let gateway = gateway.clone();
        use_hook(move || gateway.send(ClientMessage::FetchOfferedAgents));
    }
    let consent = use_memo(move || {
        let key = looked_up()?;
        let json = state
            .read()
            .app_manifest_events
            .get(&key)
            .cloned()
            .flatten()?;
        let manifest = super::integrations::read_manifest(&json, &key).ok()?;
        Some((key, manifest, json))
    });
    let installed: HashSet<String> = state
        .read()
        .integrations
        .get(&guild_id)
        .map(|bots| bots.iter().map(|b| b.bot_pubkey.clone()).collect())
        .unwrap_or_default();
    let offered = state.read().offered_agents.clone();
    if offered.is_empty() {
        return rsx! {};
    }
    rsx! {
        div {
            div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                {crate::i18n::text("Offered by this server", &[])}
            }
            for a in offered.iter().cloned() {
                div { key: "{a.pubkey}", class: "flex items-center gap-2 py-1",
                    div { class: "flex-1 min-w-0",
                        div { class: "text-xs text-[var(--text)] truncate", "{a.name}" }
                        div { class: "text-[10px] text-[var(--text-dim)] truncate",
                            if a.guild_daily_tokens > 0 {
                                {crate::i18n::text("{0} · {1} tokens a day for this guild", &[a.model.clone(), a.guild_daily_tokens.to_string()])}
                            } else {
                                "{a.model}"
                            }
                        }
                    }
                    if installed.contains(&a.pubkey) {
                        span { class: "text-[10px] text-[var(--text-dim)]", {crate::i18n::text("Installed", &[])} }
                    } else {
                        button {
                            class: BUTTON,
                            onclick: {
                                let (gateway, key) = (gateway.clone(), a.pubkey.clone());
                                move |_| {
                                    gateway.send(ClientMessage::FetchAppManifest { bot_pubkey: key.clone() });
                                    looked_up.set(Some(key.clone()));
                                }
                            },
                            {crate::i18n::text("Add here", &[])}
                        }
                    }
                }
            }
            if let Some((bot_pubkey, manifest, event)) = consent() {
                if !installed.contains(&bot_pubkey) {
                    super::integrations::AppConsent {
                        guild_id,
                        bot_pubkey,
                        manifest,
                        event,
                        on_done: move |_| looked_up.set(None),
                    }
                }
            }
        }
    }
}

/// In an installed app's row: the channels it may read and who may use it (#456).
#[component]
pub fn AppAccessRow(guild_id: Id, bot: crate::protocol::BotInstall) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let mut open = use_signal(|| false);
    let mut channels = use_signal(|| bot.access.channels.clone());
    let mut roles = use_signal(|| bot.access.roles.clone());
    let (text_channels, guild_roles) = {
        let s = state.read();
        let chans: Vec<(Id, String)> = s
            .channels
            .iter()
            .filter(|c| c.guild_id == guild_id && c.kind == crate::protocol::ChannelKind::Text)
            .map(|c| (c.id, c.name.clone()))
            .collect();
        let roles: Vec<(Id, String)> = s
            .roles
            .get(&guild_id)
            .map(|r| r.iter().map(|r| (r.id, r.name.clone())).collect())
            .unwrap_or_default();
        (chans, roles)
    };
    let summary = match (&bot.access.channels, &bot.access.roles) {
        (None, None) => crate::i18n::text("Every channel it can see, anyone may use it", &[]),
        (Some(c), None) => {
            crate::i18n::text("{0} channels, anyone may use it", &[c.len().to_string()])
        }
        (None, Some(r)) => crate::i18n::text(
            "Every channel it can see, {0} roles may use it",
            &[r.len().to_string()],
        ),
        (Some(c), Some(r)) => crate::i18n::text(
            "{0} channels, {1} roles may use it",
            &[c.len().to_string(), r.len().to_string()],
        ),
    };
    let save = {
        let (gateway, bot_pubkey) = (gateway.clone(), bot.bot_pubkey.clone());
        move |_| {
            gateway.send(ClientMessage::SetAppAccess {
                guild_id,
                bot_pubkey: bot_pubkey.clone(),
                access: crate::protocol::AppAccess {
                    channels: channels.peek().clone(),
                    roles: roles.peek().clone(),
                },
            });
            open.set(false);
        }
    };
    rsx! {
        div { class: "space-y-1.5",
            div { class: "flex items-center gap-2",
                div { class: "flex-1 min-w-0 text-[10px] text-[var(--text-dim)] truncate", "{summary}" }
                button {
                    class: "text-[10px] text-[var(--accent)] hover:underline shrink-0",
                    onclick: move |_| {
                        let now = open();
                        open.set(!now)
                    },
                    {crate::i18n::text("Access", &[])}
                }
            }
            if open() {
                div { class: "rounded border border-[var(--border)] p-2 space-y-2",
                    label { class: "flex items-center gap-2 text-xs text-[var(--text)]",
                        input {
                            r#type: "checkbox",
                            checked: channels.read().is_none(),
                            onchange: move |e| channels.set(if e.checked() { None } else { Some(Vec::new()) }),
                        }
                        {crate::i18n::text("Every channel it can see", &[])}
                    }
                    if channels.read().is_some() {
                        for (id, name) in text_channels.iter().cloned() {
                            label { key: "c-{id}", class: "flex items-center gap-2 pl-5 text-xs text-[var(--text)]",
                                input {
                                    r#type: "checkbox",
                                    checked: channels.read().as_ref().is_some_and(|c| c.contains(&id)),
                                    onchange: move |e| {
                                        if let Some(list) = channels.write().as_mut() {
                                            list.retain(|x| *x != id);
                                            if e.checked() {
                                                list.push(id);
                                            }
                                        }
                                    },
                                }
                                "#{name}"
                            }
                        }
                    }
                    label { class: "flex items-center gap-2 text-xs text-[var(--text)]",
                        input {
                            r#type: "checkbox",
                            checked: roles.read().is_none(),
                            onchange: move |e| roles.set(if e.checked() { None } else { Some(Vec::new()) }),
                        }
                        {crate::i18n::text("Anyone may use it", &[])}
                    }
                    if roles.read().is_some() {
                        for (id, name) in guild_roles.iter().cloned() {
                            label { key: "r-{id}", class: "flex items-center gap-2 pl-5 text-xs text-[var(--text)]",
                                input {
                                    r#type: "checkbox",
                                    checked: roles.read().as_ref().is_some_and(|r| r.contains(&id)),
                                    onchange: move |e| {
                                        if let Some(list) = roles.write().as_mut() {
                                            list.retain(|x| *x != id);
                                            if e.checked() {
                                                list.push(id);
                                            }
                                        }
                                    },
                                }
                                "{name}"
                            }
                        }
                        div { class: "pl-5 text-[10px] text-[var(--text-dim)]",
                            {crate::i18n::text("People who manage the guild always may.", &[])}
                        }
                    }
                    button { class: BUTTON, onclick: save, {crate::i18n::text("Save", &[])} }
                }
            }
        }
    }
}

/// In a channel's header: which AI agents read it, so nobody is surprised.
#[component]
pub fn AgentNotice() -> Element {
    let state = use_app_state();
    let readers: Vec<(String, String)> = {
        let s = state.read();
        let (Some(guild), Some(channel)) = (s.selected_guild, s.selected_channel) else {
            return rsx! {};
        };
        s.members
            .iter()
            .filter(|m| m.guild_id == guild)
            .filter(|m| m.reads.as_ref().is_none_or(|r| r.contains(&channel)))
            .filter_map(|m| {
                m.agent.as_ref().map(|a| {
                    (
                        m.user.username.clone(),
                        format!("{} ({})", a.provider, a.model),
                    )
                })
            })
            .collect()
    };
    if readers.is_empty() {
        return rsx! {};
    }
    let names = readers
        .iter()
        .map(|(n, _)| n.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let detail = readers
        .iter()
        .map(|(n, to)| {
            crate::i18n::text("{0} sends what it reads to {1}", &[n.clone(), to.clone()])
        })
        .collect::<Vec<_>>()
        .join("\n");
    rsx! {
        span {
            class: "shrink-0 whitespace-nowrap text-[10px] px-1.5 py-0.5 rounded border border-[var(--warn)]/40 text-[var(--warn)]",
            title: "{detail}",
            {crate::i18n::text("AI reads here: {0}", &[names])}
        }
    }
}
