use std::collections::HashMap;
use std::sync::Arc;

use dioxus::prelude::*;
use serde_json::{Value, json};

use crate::activity_window::{self, Bundle, FromActivity};
use crate::identity::{npub_of, truncate_pubkey};
use crate::protocol::{
    ActivityCapability, ActivityInstance, ActivityRole, ClientMessage, GuildActivity, Id,
    bundle_blob,
};
use crate::state::{AppState, GatewayTx, use_app_state, use_gateway};

pub struct BuiltinDef {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    pub caps: &'static [ActivityCapability],
    pub html: &'static str,
}

/// Player counts live in `protocol::BUILTIN_ACTIVITIES`, which the host reads.
pub const BUILTINS: &[BuiltinDef] = &[
    BuiltinDef {
        id: "dice",
        name: "Dice Roller",
        icon: "🎲",
        caps: &[
            ActivityCapability::UserRead,
            ActivityCapability::ChannelRead,
            ActivityCapability::MessageSend,
        ],
        html: DICE_HTML,
    },
    BuiltinDef {
        id: "rps",
        name: "Rock Paper Scissors",
        icon: "✊",
        caps: &[ActivityCapability::UserRead, ActivityCapability::Instance],
        html: RPS_HTML,
    },
];

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
    /// The `app` an instance names: `builtin:<id>` or the app's key.
    pub fn key(&self) -> String {
        match &self.source {
            Source::Builtin(id) => crate::protocol::builtin_activity(id),
            Source::App { bot_pubkey, .. } => bot_pubkey.clone(),
        }
    }

    pub fn shared(&self) -> bool {
        self.caps.contains(&ActivityCapability::Instance)
    }

    fn from_builtin(def: &BuiltinDef) -> Self {
        let max_players = crate::protocol::BUILTIN_ACTIVITIES
            .iter()
            .find(|(id, _)| *id == def.id)
            .map_or(1, |(_, n)| *n);
        Activity {
            name: def.name.into(),
            icon: def.icon.into(),
            caps: def.caps.to_vec(),
            max_players,
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

/// What an instance runs, if this client can run it: a built-in, or an app
/// whose bundle the host has listed.
pub fn activity_of(state: &AppState, app: &str) -> Option<Activity> {
    if let Some(id) = app.strip_prefix("builtin:") {
        return BUILTINS
            .iter()
            .find(|d| d.id == id)
            .map(Activity::from_builtin);
    }
    state
        .guild_activities
        .values()
        .flatten()
        .find(|a| a.bot_pubkey == app)
        .map(Activity::from_guild)
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

pub enum Inbound {
    Started(ActivityInstance),
    Event {
        instance_id: Id,
        from: String,
        data: String,
    },
}

thread_local! {
    static INBOX: std::cell::RefCell<Option<Coroutine<HostCmd>>> = const { std::cell::RefCell::new(None) };
}

/// From `net::apply`: the host loop owns what an instance's frames mean.
pub fn inbound(msg: Inbound) {
    INBOX.with(|i| {
        if let Some(co) = i.borrow().as_ref() {
            co.send(HostCmd::Inbound(msg));
        }
    });
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Solo,
    Start,
    Join(Id, ActivityRole),
}

#[derive(Clone, PartialEq)]
struct Pending {
    activity: Activity,
    channel: Option<Id>,
    mode: Mode,
}

#[derive(Clone, PartialEq)]
struct Launched {
    key: String,
    channel: Option<Id>,
    instance: Option<Id>,
}

/// What one open window has said and been told. Commitments are held here,
/// in Rust, so a reveal is checked by every client rather than by the page.
struct Session {
    /// Which window this is: calls a replaced window still had queued must not
    /// run under the next one's permissions and channel.
    window: u64,
    caps: Vec<ActivityCapability>,
    channel: Option<Id>,
    instance: Option<Id>,
    mine: HashMap<String, (String, String)>,
    seen: HashMap<(String, String), String>,
    last: Option<Value>,
    /// Events held until the page first calls in: before that it has no
    /// listeners, and a host's snapshot for a newcomer would be lost.
    backlog: Option<Vec<(&'static str, Value)>>,
}

impl Session {
    fn new(caps: Vec<ActivityCapability>, channel: Option<Id>, instance: Option<Id>) -> Self {
        static WINDOWS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        Session {
            window: WINDOWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            caps,
            channel,
            instance,
            mine: HashMap::new(),
            seen: HashMap::new(),
            last: None,
            backlog: Some(Vec::new()),
        }
    }

    fn tell(&mut self, name: &'static str, payload: Value) {
        match self.backlog.as_mut() {
            Some(held) if held.len() < 256 => held.push((name, payload)),
            Some(_) => {}
            None => activity_window::emit(WINDOW_KEY, name, &payload),
        }
    }
}

/// Frames for an instance whose window is still opening.
const EARLY_LIMIT: usize = 64;
const MAX_SEEN_COMMITS: usize = 1024;

enum HostCmd {
    Begin(Session),
    Window(u64, FromActivity),
    Inbound(Inbound),
    Sync(Option<ActivityInstance>),
}

/// What a commitment binds: the round, the value and a nonce only the
/// committer holds until it reveals.
pub fn commit_hash(round: &str, value: &str, nonce: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(
        format!("{round}\n{value}\n{nonce}").as_bytes(),
    ))
}

fn instance_info(state: &AppState, instance: &ActivityInstance) -> Value {
    let me = state.self_user.as_ref().map(|u| u.pubkey.clone());
    let participants: Vec<Value> = instance
        .participants
        .iter()
        .map(|p| {
            json!({
                "pubkey": p.pubkey,
                "name": state.display_name(&p.pubkey),
                "role": p.role,
            })
        })
        .collect();
    json!({
        "id": instance.id.to_string(),
        "me": me,
        "host": instance.host(),
        // A page trusts game state only from this key: players can send too.
        "app": instance.app,
        "max_players": instance.max_players,
        "participants": participants,
    })
}

fn find_instance(state: &AppState, id: Id) -> Option<ActivityInstance> {
    state
        .activity_instances
        .values()
        .flatten()
        .find(|i| i.id == id)
        .cloned()
}

fn handle_instance_call(
    session: &mut Session,
    method: &str,
    params: &Value,
    state: &Signal<AppState>,
    gateway: &GatewayTx,
) -> Option<Result<Value, String>> {
    if !method.starts_with("instance.") {
        return None;
    }
    if !session.caps.contains(&ActivityCapability::Instance) {
        return Some(Err("permission denied".into()));
    }
    let Some(instance_id) = session.instance else {
        return Some(Err("this activity was opened alone".into()));
    };
    let send = |data: Value, to: Option<String>| {
        gateway.send(ClientMessage::ActivitySend {
            instance_id,
            to,
            data: data.to_string(),
        });
    };
    let round = || {
        params
            .get("round")
            .map(|r| match r {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .filter(|r| !r.is_empty() && r.len() <= 64)
    };
    Some(match method {
        "instance.info" => find_instance(&state.read(), instance_id)
            .map(|i| instance_info(&state.read(), &i))
            .ok_or_else(|| "that activity has ended".into()),
        "instance.send" => {
            let data = params.get("data").cloned().unwrap_or(Value::Null);
            if data.get("__dxf").is_some() {
                return Some(Err("__dxf is reserved".into()));
            }
            let to = params
                .get("to")
                .and_then(|t| t.as_str())
                .map(str::to_string);
            send(data, to);
            Ok(json!({ "sent": true }))
        }
        "instance.commit" => match round() {
            None => Err("a commit needs a round".into()),
            Some(round) => {
                use rand::RngCore;
                let value = params
                    .get("value")
                    .cloned()
                    .unwrap_or(Value::Null)
                    .to_string();
                let mut nonce = [0u8; 16];
                rand::rngs::OsRng.fill_bytes(&mut nonce);
                let nonce = hex::encode(nonce);
                let hash = commit_hash(&round, &value, &nonce);
                session.mine.insert(round.clone(), (value, nonce));
                send(
                    json!({ "__dxf": "commit", "round": round, "hash": hash }),
                    None,
                );
                Ok(json!({ "committed": true }))
            }
        },
        "instance.reveal" => {
            match round().and_then(|r| session.mine.get(&r).map(|m| (r, m.clone()))) {
                None => Err("nothing was committed for that round".into()),
                Some((round, (value, nonce))) => {
                    send(
                        json!({ "__dxf": "reveal", "round": round, "value": value, "nonce": nonce }),
                        None,
                    );
                    Ok(json!({ "revealed": true }))
                }
            }
        }
        _ => Err("unknown method".into()),
    })
}

/// A frame from another participant, turned into what the page hears. A
/// reveal is only `valid` if it matches the commitment this client saw.
fn handle_instance_event(session: &mut Session, from: &str, data: &str) -> (&'static str, Value) {
    let parsed: Value = serde_json::from_str(data).unwrap_or(Value::Null);
    let round = parsed
        .get("round")
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .to_string();
    match parsed.get("__dxf").and_then(|k| k.as_str()) {
        Some("commit") => {
            let hash = parsed
                .get("hash")
                .and_then(|h| h.as_str())
                .unwrap_or("")
                .to_string();
            if session.seen.len() >= MAX_SEEN_COMMITS {
                session.seen.clear();
            }
            // A second commitment for a round is a move changed after seeing
            // the other's: the round is void for that player, not re-committed.
            session
                .seen
                .entry((from.to_string(), round.clone()))
                .and_modify(|h| h.clear())
                .or_insert(hash);
            ("instance.commit", json!({ "from": from, "round": round }))
        }
        Some("reveal") => {
            let value = parsed.get("value").and_then(|v| v.as_str()).unwrap_or("");
            let nonce = parsed.get("nonce").and_then(|v| v.as_str()).unwrap_or("");
            let valid = session
                .seen
                .remove(&(from.to_string(), round.clone()))
                .is_some_and(|h| h == commit_hash(&round, value, nonce));
            let value: Value = serde_json::from_str(value).unwrap_or(Value::Null);
            (
                "instance.reveal",
                json!({ "from": from, "round": round, "value": value, "valid": valid }),
            )
        }
        _ => ("instance.message", json!({ "from": from, "data": parsed })),
    }
}

#[component]
pub fn ActivityHost() -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

    let mut picker_open = use_signal(|| false);
    let mut consenting = use_signal(|| None::<Pending>);
    let mut pending = use_signal(|| None::<Pending>);
    let mut problem = use_signal(|| None::<String>);
    let mut launched = use_signal(|| None::<Launched>);

    let host_gw = gateway.clone();
    let co = use_coroutine(move |mut rx: UnboundedReceiver<HostCmd>| {
        let gateway = host_gw.clone();
        async move {
            use futures_util::StreamExt;
            let mut session: Option<Session> = None;
            let mut early: Vec<(Id, String, String)> = Vec::new();
            while let Some(cmd) = rx.next().await {
                match cmd {
                    HostCmd::Begin(mut next) => {
                        if let Some(old) = session.take()
                            && let Some(id) = old.instance
                            && Some(id) != next.instance
                        {
                            gateway.send(ClientMessage::ActivityLeave { instance_id: id });
                        }
                        for (instance_id, from, data) in std::mem::take(&mut early) {
                            if next.instance == Some(instance_id) {
                                let (name, payload) =
                                    handle_instance_event(&mut next, &from, &data);
                                next.tell(name, payload);
                            }
                        }
                        session = Some(next);
                    }
                    HostCmd::Window(window, FromActivity::Call { id, method, params }) => {
                        let Some(s) = session.as_mut().filter(|s| s.window == window) else {
                            continue;
                        };
                        if carries_key_material(&params) {
                            let mut st = state;
                            st.write().error_toast = Some(
                                "An activity tried to send what looks like a private key or recovery phrase. It was stopped — nothing legitimate ever asks for one.".into(),
                            );
                            activity_window::reply(
                                WINDOW_KEY,
                                id,
                                Err("refused: that looks like a key or recovery phrase".into()),
                            );
                            continue;
                        }
                        let result =
                            match handle_instance_call(s, &method, &params, &state, &gateway) {
                                Some(r) => r,
                                None => {
                                    let (ok, payload) = handle_rpc(
                                        &method, &params, &s.caps, s.channel, &state, &gateway,
                                    );
                                    if ok {
                                        Ok(payload)
                                    } else {
                                        Err(payload.as_str().unwrap_or("refused").to_string())
                                    }
                                }
                            };
                        activity_window::reply(WINDOW_KEY, id, result);
                        for (name, payload) in s.backlog.take().unwrap_or_default() {
                            activity_window::emit(WINDOW_KEY, name, &payload);
                        }
                    }
                    HostCmd::Window(window, FromActivity::Closed | FromActivity::Failed(_)) => {
                        if session.as_ref().is_none_or(|s| s.window != window) {
                            continue;
                        }
                        if let Some(s) = session.take()
                            && let Some(id) = s.instance
                        {
                            gateway.send(ClientMessage::ActivityLeave { instance_id: id });
                        }
                        launched.set(None);
                    }
                    HostCmd::Inbound(Inbound::Event {
                        instance_id,
                        from,
                        data,
                    }) => match session.as_mut() {
                        Some(s) if s.instance == Some(instance_id) => {
                            let (name, payload) = handle_instance_event(s, &from, &data);
                            s.tell(name, payload);
                        }
                        _ if pending.peek().is_some() && early.len() < EARLY_LIMIT => {
                            early.push((instance_id, from, data));
                        }
                        _ => {}
                    },
                    HostCmd::Inbound(Inbound::Started(instance)) => {
                        let waiting = pending.peek().clone();
                        if let Some(p) = waiting
                            && p.mode == Mode::Start
                            && p.activity.key() == instance.app
                        {
                            pending.set(Some(Pending {
                                mode: Mode::Join(instance.id, ActivityRole::Player),
                                ..p
                            }));
                        }
                    }
                    HostCmd::Sync(found) => {
                        let Some(s) = session.as_mut() else { continue };
                        match found {
                            None => {
                                session = None;
                                activity_window::close(WINDOW_KEY);
                                launched.set(None);
                            }
                            Some(instance) => {
                                let info = instance_info(&state.peek(), &instance);
                                if s.last.as_ref() != Some(&info) {
                                    s.last = Some(info.clone());
                                    s.tell("instance.update", info);
                                }
                            }
                        }
                    }
                }
            }
        }
    });
    use_hook(move || INBOX.with(|i| *i.borrow_mut() = Some(co)));
    use_drop(move || {
        INBOX.with(|i| *i.borrow_mut() = None);
        activity_window::close(WINDOW_KEY);
    });

    use_effect(move || {
        let Some(l) = launched.read().clone() else {
            return;
        };
        let s = state.read();
        if let Some(channel) = l.channel
            && !s.channels.iter().any(|c| c.id == channel)
        {
            activity_window::close(WINDOW_KEY);
            drop(s);
            launched.set(None);
            return;
        }
        if let Some(id) = l.instance {
            co.send(HostCmd::Sync(find_instance(&s, id)));
        }
    });

    let watch_gw = gateway.clone();
    // The memo, not `state`, is what the effect reads: the answer writes state,
    // and an effect subscribed to all of it asked again forever.
    let selected = use_memo(move || state.read().selected_channel);
    use_effect(move || {
        if let Some(channel_id) = selected() {
            watch_gw.send(ClientMessage::FetchActivityInstances { channel_id });
        }
    });

    let open_window = use_callback(
        move |(p, instance, sha, bundle): (Pending, Option<Id>, String, Bundle)| {
            let (tx, mut wrx) = tokio::sync::mpsc::unbounded_channel();
            let session = Session::new(p.activity.caps.clone(), p.channel, instance);
            let window = session.window;
            co.send(HostCmd::Begin(session));
            activity_window::open(activity_window::OpenSpec {
                key: WINDOW_KEY,
                title: p.activity.title(),
                host: bundle_host(&sha),
                bundle: Arc::new(bundle),
                tx,
            });
            launched.set(Some(Launched {
                key: p.activity.key(),
                channel: p.channel,
                instance,
            }));
            spawn(async move {
                while let Some(msg) = wrx.recv().await {
                    co.send(HostCmd::Window(window, msg));
                }
            });
        },
    );

    let fetch_gw = gateway.clone();
    let begin = use_callback(move |p: Pending| {
        problem.set(None);
        if let Source::App { sha256, .. } = &p.activity.source {
            fetch_gw.send(ClientMessage::FetchEmoji {
                images: vec![bundle_blob(sha256)],
            });
        }
        if p.mode == Mode::Start
            && let Some(channel_id) = p.channel
        {
            fetch_gw.send(ClientMessage::ActivityStart {
                channel_id,
                app: p.activity.key(),
            });
        }
        if let Mode::Join(instance_id, role) = p.mode {
            fetch_gw.send(ClientMessage::ActivityJoin { instance_id, role });
        }
        pending.set(Some(p.clone()));
        // A refusal (a full game, a hidden channel) comes back as a plain
        // error, so a launch that never resolves is given up on.
        spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            if pending
                .peek()
                .as_ref()
                .is_some_and(|q| q.activity == p.activity)
            {
                pending.set(None);
                problem.set(Some(
                    "The activity didn't open. The game may be full or over.".into(),
                ));
            }
        });
    });

    // A pending launch opens once its bundle is in hand and, for a shared one,
    // once the host lists us in the instance.
    use_effect(move || {
        let Some(p) = pending() else { return };
        let s = state.read();
        let instance = match p.mode {
            Mode::Solo => None,
            Mode::Start => return,
            Mode::Join(id, _) => {
                let me = s
                    .self_user
                    .as_ref()
                    .map(|u| u.pubkey.clone())
                    .unwrap_or_default();
                match find_instance(&s, id) {
                    Some(i) if i.role_of(&me).is_some() => Some(id),
                    _ => return,
                }
            }
        };
        let ready = match &p.activity.source {
            Source::Builtin(id) => builtin_bundle(id).ok_or_else(|| "unknown activity".to_string()),
            Source::App { sha256, .. } => {
                let Some(data_url) = s.emoji_images.get(&bundle_blob(sha256)).cloned() else {
                    return;
                };
                if data_url.is_empty() {
                    Err("This host no longer has that activity.".to_string())
                } else {
                    crate::activity_bundle::verified_bytes(&data_url, sha256)
                        .and_then(|b| crate::activity_bundle::unpack(&b))
                        .map(|b| (sha256.clone(), b))
                }
            }
        };
        drop(s);
        pending.set(None);
        match ready {
            Ok((sha, bundle)) => open_window.call((p, instance, sha, bundle)),
            Err(e) => problem.set(Some(e)),
        }
    });

    let list_gw = gateway.clone();
    let activities = available(&state.read());
    let running: Vec<ActivityInstance> = state
        .read()
        .selected_channel
        .and_then(|c| state.read().activity_instances.get(&c).cloned())
        .unwrap_or_default();
    let my_instance = launched().and_then(|l| l.instance);

    rsx! {
        div { class: "fixed bottom-3 left-3 z-40 flex flex-col items-start gap-1.5",
            for instance in running.iter().filter(|i| Some(i.id) != my_instance).cloned() {
                InstanceCard {
                    key: "{instance.id}",
                    instance,
                    on_join: move |(activity, instance_id, role): (Activity, Id, ActivityRole)| {
                        let channel = state.peek().selected_channel;
                        consenting.set(Some(Pending { activity, channel, mode: Mode::Join(instance_id, role) }));
                        picker_open.set(true);
                    },
                }
            }
            button {
                class: "border border-[var(--border)] rounded px-3 py-1 text-[10px] uppercase tracking-wider bg-[var(--panel)] hover:border-[var(--accent)] text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
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
                        if pending().is_some() {
                            div { class: "px-2 pb-2 text-[11px] text-[var(--text-dim)]", "Opening the activity…" }
                        }
                        if let Some(p) = consenting() {
                            ConsentPanel {
                                activity: p.activity.clone(),
                                joining: matches!(p.mode, Mode::Join(..)),
                                on_cancel: move |_| consenting.set(None),
                                on_approve: move |_| {
                                    if let Some(p) = consenting.peek().clone() {
                                        begin.call(p);
                                    }
                                    consenting.set(None);
                                    picker_open.set(false);
                                },
                            }
                        } else {
                            for activity in activities {
                                ActivityRow {
                                    key: "{activity.key()}",
                                    activity: activity.clone(),
                                    on_pick: move |a: Activity| {
                                        let channel = state.peek().selected_channel;
                                        let mode = if a.shared() && channel.is_some() { Mode::Start } else { Mode::Solo };
                                        consenting.set(Some(Pending { activity: a, channel, mode }));
                                    },
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
fn InstanceCard(
    instance: ActivityInstance,
    on_join: EventHandler<(Activity, Id, ActivityRole)>,
) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let activity = activity_of(&state.read(), &instance.app);
    use_hook(|| {
        if activity.is_none()
            && let Some(guild_id) = state.peek().selected_guild
        {
            gateway.send(ClientMessage::FetchGuildActivities { guild_id });
        }
    });
    let Some(activity) = activity else {
        return rsx! {};
    };
    let host = instance
        .host()
        .map(|h| state.read().display_name(h))
        .unwrap_or_default();
    let players = instance.players();
    let watching = instance.participants.len() - players;
    let full = players >= instance.max_players as usize;
    let (play, watch) = (activity.clone(), activity.clone());
    let id = instance.id;
    rsx! {
        div { class: "dxf-pop-in flex items-center gap-2 rounded-md border border-[var(--border)] bg-[var(--panel-solid)] px-2.5 py-1.5 shadow-lg max-w-xs",
            span { class: "text-base", "{activity.icon}" }
            div { class: "min-w-0 flex-1",
                div { class: "text-[11px] text-[var(--text)] truncate", "{host} is playing {activity.name}" }
                div { class: "text-[10px] text-[var(--text-dim)]",
                    "{players}/{instance.max_players} playing"
                    if watching > 0 { " · {watching} watching" }
                }
            }
            if !full {
                button {
                    class: "rounded px-2 py-0.5 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                    onclick: move |_| on_join.call((play.clone(), id, ActivityRole::Player)),
                    "Join"
                }
            }
            button {
                class: "rounded px-2 py-0.5 text-[10px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                onclick: move |_| on_join.call((watch.clone(), id, ActivityRole::Spectator)),
                "Watch"
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
                    if activity.max_players > 1 { "up to {activity.max_players} players · " }
                    "{activity.caps.len()} permission(s)"
                }
            }
        }
    }
}

#[component]
fn ConsentPanel(
    activity: Activity,
    joining: bool,
    on_cancel: EventHandler<()>,
    on_approve: EventHandler<()>,
) -> Element {
    let author = match &activity.source {
        Source::App { bot_pubkey, .. } => Some(truncate_pubkey(&npub_of(bot_pubkey))),
        Source::Builtin(_) => None,
    };
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
                    onclick: move |_| on_approve.call(()),
                    if joining { "Join" } else { "Launch" }
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

fn handle_rpc(
    method: &str,
    params: &Value,
    caps: &[ActivityCapability],
    bound_channel: Option<Id>,
    state: &Signal<AppState>,
    gateway: &GatewayTx,
) -> (bool, Value) {
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

const RPS_HTML: &str = r##"<!doctype html><html><head><meta charset="utf-8"><style>
  body { margin:0; font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;
    background:#15110d; color:#e8e2da; height:100vh; display:flex; flex-direction:column;
    align-items:center; justify-content:center; gap:14px; text-align:center; }
  .score { display:flex; gap:28px; font-size:13px; color:#9a8c7c; }
  .score b { display:block; font-size:30px; color:#e8e2da; }
  .picks { display:flex; gap:10px; }
  button { font-size:34px; width:72px; height:72px; border-radius:14px; border:1px solid #5a4634;
    background:transparent; cursor:pointer; transition:all .15s ease; }
  button:hover:not(:disabled) { border-color:#e0a06a; transform:translateY(-2px); }
  button:disabled { opacity:.35; cursor:default; }
  button.mine { border-color:#e0a06a; opacity:1; }
  .status { font-size:14px; min-height:20px; }
  .who { font-size:12px; color:#9a8c7c; min-height:16px; }
</style></head><body>
  <div class="score" id="score"></div>
  <div class="status" id="status">Connecting…</div>
  <div class="picks" id="picks">
    <button data-pick="rock">✊</button><button data-pick="paper">✋</button><button data-pick="scissors">✌️</button>
  </div>
  <div class="who" id="who"></div>
  <script>
    const FACE = { rock: '✊', paper: '✋', scissors: '✌️' };
    const BEATS = { rock: 'scissors', paper: 'rock', scissors: 'paper' };
    const REVEAL_WITHIN_MS = 15000;
    let info = null, round = 1, score = {}, mine = null;
    let committed = {}, revealed = {}, revealing = false, deadline = null, settled = false;
    const $ = (id) => document.getElementById(id);

    function players() { return info ? info.participants.filter(p => p.role === 'player') : []; }
    function nameOf(k) { const p = info && info.participants.find(p => p.pubkey === k); return p ? p.name : 'someone'; }
    function amPlayer() { return players().some(p => p.pubkey === info.me); }
    function opponent() { const o = players().find(p => p.pubkey !== info.me); return o && o.pubkey; }

    function render(message) {
      const ps = players();
      $('score').innerHTML = ps.map(p => '<div><b>' + (score[p.pubkey] || 0) + '</b>' + escape(p.name) + '</div>').join('');
      const waiting = ps.length < 2;
      document.querySelectorAll('[data-pick]').forEach(b => {
        b.disabled = !amPlayer() || waiting || !!mine;
        b.classList.toggle('mine', b.dataset.pick === mine);
      });
      $('picks').style.display = amPlayer() ? 'flex' : 'none';
      if (message) { $('status').textContent = message; }
      else if (waiting) { $('status').textContent = 'Waiting for a second player…'; }
      else if (!amPlayer()) { $('status').textContent = 'Watching round ' + round; }
      else if (!mine) { $('status').textContent = 'Round ' + round + ' — pick one'; }
      const watchers = info ? info.participants.length - ps.length : 0;
      $('who').textContent = watchers ? watchers + ' watching' : '';
    }
    function escape(t) { const d = document.createElement('div'); d.textContent = t; return d.innerHTML; }

    function nextRound(message) {
      settled = true;
      clearTimeout(deadline);
      render(message);
      setTimeout(() => {
        round += 1; mine = null; committed = {}; revealed = {}; revealing = false; settled = false;
        render();
      }, 2200);
    }

    function award(winner, message) {
      if (settled) return;
      if (winner) score[winner] = (score[winner] || 0) + 1;
      nextRound(message);
    }

    function maybeReveal() {
      const ps = players();
      if (revealing || ps.length < 2 || !ps.every(p => committed[p.pubkey])) return;
      revealing = true;
      if (amPlayer()) discordia.call('instance.reveal', { round: String(round) }).catch(() => {});
      if (mine) revealed[info.me] = mine;
      deadline = setTimeout(() => {
        const missing = players().find(p => !revealed[p.pubkey]);
        const other = missing && players().find(p => p.pubkey !== missing.pubkey);
        if (missing) award(other && other.pubkey, nameOf(missing.pubkey) + " didn't reveal — round to " + nameOf(other.pubkey));
      }, REVEAL_WITHIN_MS);
      maybeResolve();
    }

    function maybeResolve() {
      const [a, b] = players();
      if (!a || !b || !revealed[a.pubkey] || !revealed[b.pubkey]) return;
      const pa = revealed[a.pubkey], pb = revealed[b.pubkey];
      const shown = FACE[pa] + ' vs ' + FACE[pb] + ' — ';
      if (pa === pb) award(null, shown + 'draw');
      else if (BEATS[pa] === pb) award(a.pubkey, shown + nameOf(a.pubkey) + ' wins the round');
      else award(b.pubkey, shown + nameOf(b.pubkey) + ' wins the round');
    }

    document.querySelectorAll('[data-pick]').forEach(b => b.addEventListener('click', () => {
      if (mine || !amPlayer()) return;
      mine = b.dataset.pick;
      render('You picked ' + FACE[mine] + ' — waiting for ' + nameOf(opponent()));
      discordia.call('instance.commit', { round: String(round), value: mine })
        .then(() => { committed[info.me] = true; maybeReveal(); })
        .catch(() => { mine = null; render(); });
    }));

    discordia.on('instance.commit', (e) => {
      if (e.round !== String(round)) return;
      committed[e.from] = true;
      if (!amPlayer()) render(nameOf(e.from) + ' has picked');
      maybeReveal();
    });
    discordia.on('instance.reveal', (e) => {
      if (e.round !== String(round)) return;
      if (!e.valid || !FACE[e.value]) {
        const other = players().find(p => p.pubkey !== e.from);
        award(other && other.pubkey, nameOf(e.from) + "'s move didn't match their commitment — round to " + nameOf(other && other.pubkey));
        return;
      }
      revealed[e.from] = e.value;
      maybeResolve();
    });
    discordia.on('instance.message', (e) => {
      const d = e.data || {};
      if (d.type === 'snapshot' && info && e.from === info.host) {
        score = d.score || {}; round = d.round || 1; render();
      }
    });
    discordia.on('instance.update', (next) => {
      const before = info;
      info = next;
      if (before) {
        const gone = before.participants.filter(p => p.role === 'player' && !next.participants.some(q => q.pubkey === p.pubkey));
        const arrived = next.participants.filter(p => !before.participants.some(q => q.pubkey === p.pubkey));
        if (next.host === next.me) {
          arrived.forEach(p => discordia.call('instance.send', { to: p.pubkey, data: { type: 'snapshot', score: score, round: round } }).catch(() => {}));
        }
        if (gone.length && Object.keys(committed).length && !settled) {
          const stayer = players()[0];
          award(stayer && stayer.pubkey, gone[0].name + ' left — round to ' + (stayer ? stayer.name : 'nobody'));
          return;
        }
      }
      if (!settled) render();
    });

    discordia.call('instance.info').then((i) => { info = i; render(); })
      .catch((e) => { $('status').textContent = 'This game has ended.'; });
  </script>
</body></html>"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn session(instance: Id) -> Session {
        Session::new(vec![ActivityCapability::Instance], None, Some(instance))
    }

    #[tokio::test]
    async fn a_reveal_is_valid_only_against_the_commitment_every_client_saw() {
        fn harness() -> Element {
            let state = use_signal(AppState::empty);
            use_hook(move || {
                let id = Id::new_v4();
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let gateway = GatewayTx(tx);
                let (mut alice, mut bob) = (session(id), session(id));
                let mut sent = || match rx.try_recv().unwrap() {
                    ClientMessage::ActivitySend { data, .. } => data,
                    other => panic!("{other:?}"),
                };
                let call = |s: &mut Session, m: &str, p: Value| {
                    handle_instance_call(s, m, &p, &state, &gateway).unwrap()
                };

                assert!(call(&mut alice, "instance.reveal", json!({ "round": "1" })).is_err());
                call(
                    &mut alice,
                    "instance.commit",
                    json!({ "round": "1", "value": "rock" }),
                )
                .unwrap();
                let commit = sent();
                assert!(
                    !commit.contains("rock"),
                    "a commitment hides the move: {commit}"
                );
                assert_eq!(
                    handle_instance_event(&mut bob, "alice", &commit).0,
                    "instance.commit"
                );

                call(&mut alice, "instance.reveal", json!({ "round": "1" })).unwrap();
                let reveal = sent();
                let (name, heard) = handle_instance_event(&mut bob, "alice", &reveal);
                assert_eq!(name, "instance.reveal");
                assert_eq!(heard["valid"], json!(true));
                assert_eq!(heard["value"], json!("rock"));

                call(
                    &mut alice,
                    "instance.commit",
                    json!({ "round": "2", "value": "rock" }),
                )
                .unwrap();
                handle_instance_event(&mut bob, "alice", &sent());
                call(&mut alice, "instance.reveal", json!({ "round": "2" })).unwrap();
                let tampered = sent().replace("rock", "paper");
                assert_eq!(
                    handle_instance_event(&mut bob, "alice", &tampered).1["valid"],
                    json!(false)
                );

                // A changed mind: a second commit after seeing the other's move voids the round.
                call(
                    &mut alice,
                    "instance.commit",
                    json!({ "round": "3", "value": "rock" }),
                )
                .unwrap();
                handle_instance_event(&mut bob, "alice", &sent());
                call(
                    &mut alice,
                    "instance.commit",
                    json!({ "round": "3", "value": "paper" }),
                )
                .unwrap();
                handle_instance_event(&mut bob, "alice", &sent());
                call(&mut alice, "instance.reveal", json!({ "round": "3" })).unwrap();
                assert_eq!(
                    handle_instance_event(&mut bob, "alice", &sent()).1["valid"],
                    json!(false)
                );

                let unseen =
                    json!({ "__dxf": "reveal", "round": "9", "value": "\"rock\"", "nonce": "00" });
                assert_eq!(
                    handle_instance_event(&mut bob, "mallory", &unseen.to_string()).1["valid"],
                    json!(false)
                );

                assert!(
                    call(
                        &mut alice,
                        "instance.send",
                        json!({ "data": { "__dxf": "reveal" } })
                    )
                    .is_err()
                );
                call(
                    &mut alice,
                    "instance.send",
                    json!({ "data": { "move": 1 } }),
                )
                .unwrap();
                assert_eq!(
                    handle_instance_event(&mut bob, "alice", &sent()).0,
                    "instance.message"
                );

                let mut alone = session(id);
                alone.instance = None;
                assert!(
                    handle_instance_call(&mut alone, "instance.info", &json!({}), &state, &gateway)
                        .unwrap()
                        .is_err()
                );
                let mut no_cap = session(id);
                no_cap.caps.clear();
                assert!(
                    handle_instance_call(
                        &mut no_cap,
                        "instance.send",
                        &json!({}),
                        &state,
                        &gateway
                    )
                    .unwrap()
                    .is_err()
                );
                assert!(
                    handle_instance_call(&mut no_cap, "user.get", &json!({}), &state, &gateway)
                        .is_none()
                );
            });
            rsx! {}
        }
        VirtualDom::new(harness).rebuild_in_place();
    }

    #[tokio::test]
    async fn unrelated_state_changes_do_not_refetch_the_channels_activities() {
        #[derive(Clone)]
        struct Props(tokio::sync::mpsc::UnboundedSender<ClientMessage>);
        impl PartialEq for Props {
            fn eq(&self, _: &Self) -> bool {
                true
            }
        }
        fn harness(props: Props) -> Element {
            use_context_provider(|| GatewayTx(props.0.clone()));
            let mut state = use_context_provider(|| {
                let mut s = AppState::empty();
                s.selected_channel = Some(Id::new_v4());
                Signal::new(s)
            });
            use_future(move || async move {
                for i in 0..20 {
                    state.write().error_toast = Some(i.to_string());
                    tokio::task::yield_now().await;
                }
            });
            rsx! { ActivityHost {} }
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut dom = VirtualDom::new_with_props(harness, Props(tx));
        dom.rebuild_in_place();
        for _ in 0..60 {
            if tokio::time::timeout(std::time::Duration::from_millis(20), dom.wait_for_work())
                .await
                .is_err()
            {
                break;
            }
            dom.render_immediate(&mut dioxus_core::NoOpMutations);
        }
        let mut fetches = 0;
        while let Ok(msg) = rx.try_recv() {
            if matches!(msg, ClientMessage::FetchActivityInstances { .. }) {
                fetches += 1;
            }
        }
        assert_eq!(
            fetches, 1,
            "one fetch per channel selected, not one per state change"
        );
    }

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
