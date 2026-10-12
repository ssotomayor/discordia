use dioxus::prelude::*;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::activity_window::{self, Bundle, FromActivity};
use crate::identity::{npub_of, truncate_pubkey};
use crate::protocol::{ActivityCapability, ClientMessage, GuildActivity, Id, bundle_blob};
use crate::state::{AppState, GatewayTx, use_app_state, use_gateway};

pub struct BuiltinDef {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    pub caps: &'static [ActivityCapability],
    pub max_players: u8,
    pub html: &'static str,
}

pub const BUILTINS: &[BuiltinDef] = &[BuiltinDef {
    id: "dice",
    name: "Dice Roller",
    icon: "🎲",
    caps: &[
        ActivityCapability::UserRead,
        ActivityCapability::ChannelRead,
        ActivityCapability::MessageSend,
    ],
    max_players: 1,
    html: DICE_HTML,
}];

#[derive(Clone, PartialEq, Debug)]
pub enum Source {
    Builtin(&'static str),
    App { bot_pubkey: String, sha256: String },
}

#[derive(Clone, PartialEq, Debug)]
pub struct Activity {
    pub name: String,
    pub icon: String,
    pub caps: Vec<ActivityCapability>,
    pub max_players: u8,
    pub source: Source,
}

impl Activity {
    pub fn key(&self) -> String {
        match &self.source {
            Source::Builtin(id) => format!("builtin:{id}"),
            Source::App { bot_pubkey, .. } => bot_pubkey.clone(),
        }
    }

    fn from_builtin(def: &BuiltinDef) -> Self {
        Activity {
            name: def.name.into(),
            icon: def.icon.into(),
            caps: def.caps.to_vec(),
            max_players: def.max_players,
            source: Source::Builtin(def.id),
        }
    }

    fn from_guild(a: &GuildActivity) -> Self {
        Activity {
            name: a.name.clone(),
            icon: a.icon.clone(),
            caps: a.capabilities.clone(),
            max_players: a.max_players,
            source: Source::App {
                bot_pubkey: a.bot_pubkey.clone(),
                sha256: a.sha256.clone(),
            },
        }
    }

    /// The title bar is drawn by the OS, outside the page, so an activity
    /// cannot paint over who made it.
    fn title(&self) -> String {
        match &self.source {
            Source::Builtin(_) => format!("{} {} — activity", self.icon, self.name),
            Source::App { bot_pubkey, .. } => format!(
                "{} {} — third-party activity by {}",
                self.icon,
                self.name,
                truncate_pubkey(&npub_of(bot_pubkey))
            ),
        }
    }
}

pub fn available(state: &AppState) -> Vec<Activity> {
    let mut out: Vec<Activity> = BUILTINS.iter().map(Activity::from_builtin).collect();
    if let Some(guild) = state.selected_guild
        && let Some(list) = state.guild_activities.get(&guild)
    {
        out.extend(list.iter().map(Activity::from_guild));
    }
    out
}

/// One activity window per client; a second launch replaces the first.
const WINDOW_KEY: u64 = 1;

/// The page's origin: two activities never share storage.
pub fn bundle_host(sha256_hex: &str) -> String {
    sha256_hex[..32].to_string()
}

fn builtin_bundle(id: &str) -> Option<(String, Bundle)> {
    use sha2::{Digest, Sha256};
    let def = BUILTINS.iter().find(|d| d.id == id)?;
    let sha = hex::encode(Sha256::digest(def.html.as_bytes()));
    Some((sha, Bundle::single_page(def.html)))
}

#[component]
pub fn ActivityHost() -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

    let mut picker_open = use_signal(|| false);
    let mut consenting = use_signal(|| None::<Activity>);
    let mut loading = use_signal(|| None::<(Activity, Option<Id>)>);
    let mut problem = use_signal(|| None::<String>);
    let mut launched = use_signal(|| None::<Launched>);

    use_drop(|| activity_window::close(WINDOW_KEY));

    use_effect(move || {
        let Some(channel) = launched.read().as_ref().and_then(|l| l.channel) else {
            return;
        };
        if !state.read().channels.iter().any(|c| c.id == channel) {
            activity_window::close(WINDOW_KEY);
            launched.set(None);
        }
    });

    let window_gw = gateway.clone();
    let open_window = use_callback(
        move |(activity, channel, sha, bundle): (Activity, Option<Id>, String, Bundle)| {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            activity_window::open(activity_window::OpenSpec {
                key: WINDOW_KEY,
                title: activity.title(),
                host: bundle_host(&sha),
                bundle: Arc::new(bundle),
                tx,
            });
            let key = activity.key();
            launched.set(Some(Launched {
                key: key.clone(),
                channel,
            }));
            let gateway = window_gw.clone();
            let caps = activity.caps.clone();
            spawn(async move {
                while let Some(msg) = rx.recv().await {
                    match msg {
                        FromActivity::Call { id, method, params } => {
                            let (ok, payload) =
                                handle_rpc(&method, &params, &caps, channel, &state, &gateway);
                            let result = if ok {
                                Ok(payload)
                            } else {
                                Err(payload.as_str().unwrap_or("refused").to_string())
                            };
                            activity_window::reply(WINDOW_KEY, id, result);
                        }
                        FromActivity::Closed | FromActivity::Failed(_) => {
                            if launched.peek().as_ref().map(|l| l.key.as_str())
                                == Some(key.as_str())
                            {
                                launched.set(None);
                            }
                            break;
                        }
                    }
                }
            });
        },
    );

    let fetch_gw = gateway.clone();
    let launch = use_callback(move |activity: Activity| {
        let channel = state.read().selected_channel;
        problem.set(None);
        match &activity.source {
            Source::Builtin(id) => {
                if let Some((sha, bundle)) = builtin_bundle(id) {
                    open_window.call((activity.clone(), channel, sha, bundle));
                }
            }
            Source::App { .. } => {
                loading.set(Some((activity.clone(), channel)));
                if let Source::App { sha256, .. } = &activity.source {
                    fetch_gw.send(ClientMessage::FetchEmoji {
                        images: vec![bundle_blob(sha256)],
                    });
                }
            }
        }
    });

    use_effect(move || {
        let Some((activity, channel)) = loading() else {
            return;
        };
        let Source::App { sha256, .. } = &activity.source else {
            return;
        };
        let blob = bundle_blob(sha256);
        let Some(data_url) = state.read().emoji_images.get(&blob).cloned() else {
            return;
        };
        loading.set(None);
        if data_url.is_empty() {
            problem.set(Some("This host no longer has that activity.".into()));
            return;
        }
        match crate::activity_bundle::verified_bytes(&data_url, sha256)
            .and_then(|bytes| crate::activity_bundle::unpack(&bytes))
        {
            Ok(bundle) => open_window.call((activity.clone(), channel, sha256.clone(), bundle)),
            Err(e) => problem.set(Some(e)),
        }
    });

    let list_gw = gateway.clone();
    let activities = available(&state.read());

    rsx! {
        button {
            class: "fixed bottom-3 left-3 z-40 border border-[var(--border)] rounded px-3 py-1 text-[10px] uppercase tracking-wider bg-[var(--panel)] hover:border-[var(--accent)] text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
            onclick: move |_| {
                if launched.peek().is_some() && activity_window::is_open(WINDOW_KEY) {
                    activity_window::focus(WINDOW_KEY);
                }
                if !picker_open()
                    && let Some(guild_id) = state.peek().selected_guild
                {
                    list_gw.send(ClientMessage::FetchGuildActivities { guild_id });
                }
                picker_open.set(!picker_open());
            },
            "Activities"
        }

        if picker_open() {
            div {
                class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
                onclick: move |_| { picker_open.set(false); consenting.set(None); },
                div {
                    class: "dxf-modal-in w-80 bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                    onclick: move |e| e.stop_propagation(),
                    div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                        h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", "Activities" }
                        button {
                            class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                            onclick: move |_| { picker_open.set(false); consenting.set(None); },
                            "✕"
                        }
                    }
                    div { class: "p-2",
                        if let Some(err) = problem() {
                            div { class: "px-2 pb-2 text-[11px] text-[var(--danger)]", "{err}" }
                        }
                        if loading().is_some() {
                            div { class: "px-2 pb-2 text-[11px] text-[var(--text-dim)]", "Loading the activity…" }
                        }
                        if let Some(activity) = consenting() {
                            ConsentPanel {
                                activity,
                                on_cancel: move |_| consenting.set(None),
                                on_approve: move |a: Activity| {
                                    launch.call(a);
                                    consenting.set(None);
                                    picker_open.set(false);
                                },
                            }
                        } else {
                            for activity in activities {
                                ActivityRow {
                                    key: "{activity.key()}",
                                    activity: activity.clone(),
                                    on_pick: move |a: Activity| consenting.set(Some(a)),
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ActivityRow(activity: Activity, on_pick: EventHandler<Activity>) -> Element {
    let third_party = matches!(activity.source, Source::App { .. });
    let pick = activity.clone();
    rsx! {
        button {
            class: "w-full flex items-center gap-3 px-2 py-2 rounded hover:bg-white/[0.03] text-left transition-colors",
            onclick: move |_| on_pick.call(pick.clone()),
            span { class: "w-9 h-9 rounded-md border border-[var(--border)] flex items-center justify-center text-lg shrink-0",
                "{activity.icon}"
            }
            div { class: "flex-1 min-w-0",
                div { class: "text-sm text-[var(--text)] truncate", "{activity.name}" }
                div { class: "text-[10px] text-[var(--text-dim)]",
                    if third_party { "Installed app · " } else { "Built in · " }
                    "{activity.caps.len()} permission(s)"
                }
            }
        }
    }
}

#[component]
fn ConsentPanel(
    activity: Activity,
    on_cancel: EventHandler<()>,
    on_approve: EventHandler<Activity>,
) -> Element {
    let author = match &activity.source {
        Source::App { bot_pubkey, .. } => Some(truncate_pubkey(&npub_of(bot_pubkey))),
        Source::Builtin(_) => None,
    };
    let approve = activity.clone();
    rsx! {
        div { class: "px-1 py-1",
            div { class: "flex items-center gap-2 mb-2",
                span { class: "text-lg", "{activity.icon}" }
                div { class: "min-w-0",
                    div { class: "text-sm text-[var(--text)] font-medium truncate", "{activity.name}" }
                    if let Some(author) = &author {
                        div { class: "font-mono text-[10px] text-[var(--text-dim)]", "Third-party · {author}" }
                    }
                }
            }
            div { class: "text-xs text-[var(--text-muted)] mb-1.5", "This activity will be able to:" }
            ul { class: "space-y-1 mb-3",
                for cap in activity.caps.iter().copied() {
                    li { class: "flex items-start gap-2 text-xs text-[var(--text)]",
                        span { class: "text-[var(--accent)]", "•" }
                        span { "{cap.label()}" }
                    }
                }
                if activity.caps.is_empty() {
                    li { class: "text-xs text-[var(--text-dim)]", "Nothing beyond drawing in its window" }
                }
            }
            div { class: "text-[10px] text-[var(--text-dim)] mb-2",
                "Opens in its own window with no internet access. It can't read your other channels, files, or keys, and it never needs your recovery phrase."
            }
            div { class: "flex gap-2",
                button {
                    class: "flex-1 rounded px-2 py-1.5 text-[11px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                    onclick: move |_| on_approve.call(approve.clone()),
                    "Launch"
                }
                button {
                    class: "rounded px-3 py-1.5 text-[11px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                    onclick: move |_| on_cancel.call(()),
                    "Cancel"
                }
            }
        }
    }
}

#[derive(Clone, PartialEq)]
struct Launched {
    key: String,
    channel: Option<Id>,
}

fn handle_rpc(
    method: &str,
    params: &Value,
    caps: &[ActivityCapability],
    bound_channel: Option<Id>,
    state: &Signal<AppState>,
    gateway: &GatewayTx,
) -> (bool, Value) {
    if carries_key_material(params) {
        state.clone().write().error_toast = Some(
            "An activity tried to send what looks like a private key or recovery phrase. It was stopped — nothing legitimate ever asks for one.".into(),
        );
        return (
            false,
            json!("refused: that looks like a key or recovery phrase"),
        );
    }
    let has = |c: ActivityCapability| caps.contains(&c);
    match method {
        "user.get" if has(ActivityCapability::UserRead) => {
            let s = state.read();
            match &s.self_user {
                Some(u) => (true, json!({ "pubkey": u.pubkey, "username": u.username })),
                None => (false, json!("not connected")),
            }
        }
        "channel.get" if has(ActivityCapability::ChannelRead) => {
            let s = state.read();
            let cid = bound_channel;
            let name = cid.and_then(|id| {
                s.channels
                    .iter()
                    .find(|c| c.id == id)
                    .map(|c| c.name.clone())
            });
            (
                true,
                json!({ "id": cid.map(|i| i.to_string()), "name": name }),
            )
        }
        "message.send" if has(ActivityCapability::MessageSend) => {
            let content = params
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if content.is_empty() {
                return (false, json!("content is empty"));
            }
            let cid = bound_channel;
            match cid {
                Some(channel_id) => {
                    if gateway
                        .0
                        .send(ClientMessage::SendMessage {
                            image_name: None,
                            channel_id,
                            content,
                            image: None,
                            reply_to: None,
                        })
                        .is_err()
                    {
                        return (false, json!("The connection is closed"));
                    }
                    (true, json!({ "sent": true }))
                }
                None => (false, json!("no channel is open")),
            }
        }
        _ => (false, json!("permission denied or unknown method")),
    }
}

/// With no network, the bridge is an activity's only way out, so a phrase
/// it phished from the person is stopped here (#306).
pub fn carries_key_material(value: &Value) -> bool {
    match value {
        Value::String(s) => text_carries_key(s),
        Value::Array(items) => items.iter().any(carries_key_material),
        Value::Object(map) => map
            .iter()
            .any(|(k, v)| text_carries_key(k) || carries_key_material(v)),
        _ => false,
    }
}

fn text_carries_key(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let bech32 = |prefix: &str, min: usize| {
        lower.match_indices(prefix).any(|(i, _)| {
            lower[i + prefix.len()..]
                .chars()
                .take_while(|c| "qpzry9x8gf2tvdw0s3jn54khce6mua7l".contains(*c))
                .count()
                >= min
        })
    };
    if bech32("nsec1", 50) || bech32("ncryptsec1", 50) {
        return true;
    }
    let words = bip39::Language::English;
    let mut run = 0;
    for word in lower
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| !w.is_empty())
    {
        run = if words.find_word(word).is_some() {
            run + 1
        } else {
            0
        };
        if run >= 12 {
            return true;
        }
    }
    false
}

const DICE_HTML: &str = r##"<!doctype html><html><head><meta charset="utf-8"><style>
  body { margin:0; font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;
    background:#15110d; color:#e8e2da; height:100vh; display:flex; flex-direction:column;
    align-items:center; justify-content:center; gap:18px; }
  .die { font-size:84px; line-height:1; user-select:none; transition:transform .15s ease; }
  .die.rolling { transform:rotate(20deg) scale(1.1); }
  button { font:inherit; padding:8px 16px; border-radius:8px; border:1px solid #5a4634;
    background:transparent; color:#e0a06a; cursor:pointer; transition:all .15s ease; }
  button:hover { border-color:#e0a06a; }
  button:disabled { opacity:.4; cursor:default; }
  .who { font-size:13px; color:#9a8c7c; min-height:18px; }
  .row { display:flex; gap:10px; }
</style></head><body>
  <div class="who" id="who">…</div>
  <div class="die" id="die">🎲</div>
  <div class="row">
    <button id="roll">Roll</button>
    <button id="share" disabled>Share to chat</button>
  </div>
  <div class="who" id="where"></div>
  <script>
    const dxf = {
      getUser: function () { return discordia.call('user.get'); },
      getChannel: function () { return discordia.call('channel.get'); },
      sendMessage: function (content) { return discordia.call('message.send', { content: content }); }
    };

    const dieEl = document.getElementById('die');
    const rollBtn = document.getElementById('roll');
    const shareBtn = document.getElementById('share');
    const whoEl = document.getElementById('who');
    const whereEl = document.getElementById('where');
    const faces = ['⚀','⚁','⚂','⚃','⚄','⚅'];
    let last = 0;

    dxf.getUser().then(function (u) { whoEl.textContent = 'Hi, ' + u.username; })
       .catch(function () { whoEl.textContent = ''; });

    function showWhere() {
      dxf.getChannel()
         .then(function (c) { whereEl.textContent = c && c.name ? 'shares go to #' + c.name : ''; })
         .catch(function () { whereEl.textContent = ''; });
    }
    showWhere();

    rollBtn.addEventListener('click', function () {
      dieEl.classList.add('rolling');
      last = 1 + Math.floor(Math.random() * 6);
      setTimeout(function () {
        dieEl.textContent = faces[last - 1];
        dieEl.classList.remove('rolling');
        shareBtn.disabled = false;
      }, 150);
    });

    shareBtn.addEventListener('click', function () {
      if (!last) return;
      shareBtn.disabled = true;
      dxf.sendMessage('🎲 rolled a ' + last + '!')
         .then(function () { showWhere(); shareBtn.textContent = 'Shared!'; setTimeout(function(){ shareBtn.textContent='Share to chat'; shareBtn.disabled=false; }, 1200); })
         .catch(function (err) { shareBtn.textContent = 'Denied'; });
    });
  </script>
</body></html>"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_material_is_stopped_at_the_bridge() {
        let phrase =
            "abandon ability able about above absent absorb abstract absurd abuse access accident";
        assert!(carries_key_material(&json!({ "content": phrase })));
        assert!(carries_key_material(
            &json!({ "nested": [{ "x": format!("my words: {phrase}!") }] })
        ));
        let nsec = format!("nsec1{}", "q".repeat(58));
        assert!(carries_key_material(
            &json!({ "content": nsec.to_uppercase() })
        ));
        assert!(carries_key_material(
            &json!({ format!("ncryptsec1{}", "p".repeat(80)): 1 })
        ));

        assert!(!carries_key_material(
            &json!({ "content": "🎲 rolled a 6!" })
        ));
        assert!(!carries_key_material(
            &json!({ "content": "nsec1 is what a private key starts with" })
        ));
        let eleven = phrase.rsplit_once(' ').unwrap().0;
        assert!(!carries_key_material(&json!({ "content": eleven })));
    }

    #[tokio::test]
    async fn integration_requires_permission_and_shares_only_to_its_launch_channel() {
        fn harness() -> Element {
            let state = use_signal(AppState::empty);
            use_hook(move || {
                let channel = Id::new_v4();
                let mut state = state;
                state.write().selected_channel = Some(Id::new_v4());
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let gateway = GatewayTx(tx);
                let def: &[ActivityCapability] = BUILTINS[0].caps;
                let denied: &[ActivityCapability] = &[];
                let params = json!({ "content": "🎲 rolled a 6!" });
                assert!(
                    !handle_rpc(
                        "message.send",
                        &params,
                        denied,
                        Some(channel),
                        &state,
                        &gateway
                    )
                    .0
                );
                assert!(rx.try_recv().is_err());
                assert!(
                    handle_rpc(
                        "message.send",
                        &params,
                        def,
                        Some(channel),
                        &state,
                        &gateway
                    )
                    .0
                );
                assert!(
                    matches!(rx.try_recv().unwrap(), ClientMessage::SendMessage { channel_id, content, .. } if channel_id == channel && content == "🎲 rolled a 6!")
                );
                assert!(!handle_rpc("message.send", &params, def, None, &state, &gateway).0);
                assert!(
                    !handle_rpc(
                        "message.send",
                        &json!({"content":" "}),
                        def,
                        Some(channel),
                        &state,
                        &gateway
                    )
                    .0
                );
                drop(rx);
                assert!(
                    !handle_rpc(
                        "message.send",
                        &params,
                        def,
                        Some(channel),
                        &state,
                        &gateway
                    )
                    .0
                );
            });
            rsx! {}
        }
        VirtualDom::new(harness).rebuild_in_place();
    }
}
