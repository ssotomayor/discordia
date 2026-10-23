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
use crate::state::{AppState, ConnectionStatus, GatewayTx, use_app_state, use_gateway};

/// The whole guild, not the open channel: a game started elsewhere must still
/// be joinable, and pushes keep every visible channel current after this.
fn use_activity_instance_watch(state: Signal<AppState>, gateway: GatewayTx) {
    // Replies mutate AppState; subscribing to it directly would request another reply.
    let guild = use_memo(move || {
        let s = state.read();
        (s.status == ConnectionStatus::Ready && !s.dm_mode)
            .then_some(s.selected_guild)
            .flatten()
    });
    use_effect(move || {
        if let Some(guild_id) = guild() {
            gateway.send(ClientMessage::FetchGuildActivityInstances { guild_id });
        }
    });
}

pub fn open_in_guild(state: &AppState) -> Vec<(ActivityInstance, Activity)> {
    let Some(guild) = state.selected_guild else {
        return Vec::new();
    };
    state
        .channels
        .iter()
        .filter(|c| c.guild_id == guild)
        .filter_map(|c| state.activity_instances.get(&c.id))
        .flatten()
        .filter_map(|i| activity_of(state, &i.app).map(|a| (i.clone(), a)))
        .collect()
}

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
    fn display_name(&self) -> &str {
        match self.source {
            Source::Builtin(_) => crate::i18n::label(&self.name),
            Source::App { .. } => &self.name,
        }
    }

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
            Source::Builtin(_) => crate::i18n::text(
                "{0} {1} — activity",
                &[self.icon.clone(), self.display_name().to_owned()],
            ),
            Source::App { bot_pubkey, .. } => crate::i18n::text(
                "{0} {1} — third-party activity by {2}",
                &[
                    self.icon.clone(),
                    self.name.clone(),
                    truncate_pubkey(&npub_of(bot_pubkey)),
                ],
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
    builtin_bundle_in(id, crate::i18n::current())
}

fn builtin_bundle_in(id: &str, language: crate::i18n::Language) -> Option<(String, Bundle)> {
    use sha2::{Digest, Sha256};
    let def = BUILTINS.iter().find(|d| d.id == id)?;
    let translations = BUILTIN_MESSAGES
        .iter()
        .map(|key| (*key, crate::i18n::translate(language, key, &[])))
        .collect::<std::collections::BTreeMap<_, _>>();
    let dictionary = serde_json::to_string(&translations)
        .ok()?
        .replace('<', "\\u003c");
    let init = format!(
        "const translations = {dictionary}; document.documentElement.lang = '{}';",
        language.tag()
    );
    let html = def.html.replace("/* BUILTIN_TRANSLATIONS */", &init);
    let sha = hex::encode(Sha256::digest(html.as_bytes()));
    Some((sha, Bundle::single_page(&html)))
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

pub fn request_join(instance_id: Id, role: ActivityRole) {
    INBOX.with(|i| {
        if let Some(co) = i.borrow().as_ref() {
            co.send(HostCmd::Join(instance_id, role));
        }
    });
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
    Join(Id, ActivityRole),
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
pub fn ActivityHost(
    picker: Option<Signal<bool>>,
    #[props(default = false)] mobile: bool,
) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

    let local_picker = use_signal(|| false);
    let unavailable = cfg!(any(target_os = "android", target_os = "ios"));
    let mut picker_open = picker.unwrap_or(local_picker);
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
                    HostCmd::Window(
                        window,
                        event @ (FromActivity::Closed | FromActivity::Failed(_)),
                    ) => {
                        if session.as_ref().is_none_or(|s| s.window != window) {
                            continue;
                        }
                        if let Some(s) = session.take()
                            && let Some(id) = s.instance
                        {
                            gateway.send(ClientMessage::ActivityLeave { instance_id: id });
                        }
                        launched.set(None);
                        if let FromActivity::Failed(error) = event {
                            problem.set(Some(error));
                            consenting.set(None);
                            picker_open.set(true);
                        }
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
                    HostCmd::Join(instance_id, role) => {
                        if session
                            .as_ref()
                            .is_some_and(|s| s.instance == Some(instance_id))
                            && activity_window::is_open(WINDOW_KEY)
                        {
                            activity_window::focus(WINDOW_KEY);
                            continue;
                        }
                        let found = {
                            let s = state.peek();
                            find_instance(&s, instance_id)
                                .and_then(|i| activity_of(&s, &i.app).map(|a| (i.channel_id, a)))
                        };
                        let Some((channel, activity)) = found else {
                            continue;
                        };
                        consenting.set(Some(Pending {
                            activity,
                            channel: Some(channel),
                            mode: Mode::Join(instance_id, role),
                        }));
                        picker_open.set(true);
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

    use_activity_instance_watch(state, gateway.clone());

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
    use_effect(move || {
        if picker_open()
            && let Some(guild_id) = state.peek().selected_guild
        {
            list_gw.send(ClientMessage::FetchGuildActivities { guild_id });
        }
    });
    let activities = available(&state.read());
    let running: Vec<ActivityInstance> = state
        .read()
        .selected_channel
        .and_then(|c| state.read().activity_instances.get(&c).cloned())
        .unwrap_or_default();
    let my_instance = launched().and_then(|l| l.instance);

    rsx! {
        if !mobile {
          div { class: "fixed bottom-3 left-3 z-40 flex flex-col items-start gap-1.5",
            for instance in running.iter().filter(|i| Some(i.id) != my_instance).cloned() {
                InstanceCard { key: "{instance.id}", instance }
            }
            button {
                class: "border border-[var(--border)] rounded px-3 py-1 text-[10px] uppercase tracking-wider bg-[var(--panel)] hover:border-[var(--accent)] text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
                onclick: move |_| {
                    if launched.peek().is_some() && activity_window::is_open(WINDOW_KEY) {
                        activity_window::focus(WINDOW_KEY);
                    }
                    picker_open.set(!picker_open());
                },
                {crate::i18n::text("Activities", &[])}
            }
        }
        }

        if picker_open() {
            div {
                class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
                onclick: move |_| { picker_open.set(false); consenting.set(None); },
                div {
                    class: "dxf-activities-dialog dxf-modal-in w-80 bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                    onclick: move |e| e.stop_propagation(),
                    div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                        h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", {crate::i18n::text("Activities", &[])} }
                        button {
                            class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                            onclick: move |_| { picker_open.set(false); consenting.set(None); },
                            "✕"
                        }
                    }
                    div { class: "p-2",
                        if unavailable {
                            div { class: "px-2 pb-2 text-sm text-[var(--text-muted)]",
                                {crate::i18n::text("Activities, including Dice Roller and Rock Paper Scissors, are not available on mobile yet.", &[])}
                            }
                        }
                        if let Some(err) = problem() {
                            div { class: "px-2 pb-2 text-[11px] text-[var(--danger)]", {crate::i18n::message(&(err).to_string())} }
                        }
                        if pending().is_some() {
                            div { class: "px-2 pb-2 text-[11px] text-[var(--text-dim)]", {crate::i18n::text("Opening the activity…", &[])} }
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
                                        if unavailable { return; }
                                        let channel = state.peek().selected_channel;
                                        let mode = if a.shared() && channel.is_some() { Mode::Start } else { Mode::Solo };
                                        consenting.set(Some(Pending { activity: a, channel, mode }));
                                    },
                                    disabled: unavailable,
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
fn InstanceCard(instance: ActivityInstance) -> Element {
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
    let id = instance.id;
    rsx! {
        div { class: "dxf-pop-in flex items-center gap-2 rounded-md border border-[var(--border)] bg-[var(--panel-solid)] px-2.5 py-1.5 shadow-lg max-w-xs",
            span { class: "text-base", "{activity.icon}" }
            div { class: "min-w-0 flex-1",
                div { class: "text-[11px] text-[var(--text)] truncate", {crate::i18n::text("{0} is playing {1}", &[(host).to_string(), (activity . name).to_string()])} }
                div { class: "text-[10px] text-[var(--text-dim)]",
                    {crate::i18n::text("{0}/{1} playing", &[(players).to_string(), (instance . max_players).to_string()])}
                    if watching > 0 { {crate::i18n::text(" · {0} watching", &[(watching).to_string()])} }
                }
            }
            if !full {
                button {
                    class: "rounded px-2 py-0.5 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                    onclick: move |_| request_join(id, ActivityRole::Player),
                    {crate::i18n::text("Join", &[])}
                }
            }
            button {
                class: "rounded px-2 py-0.5 text-[10px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                onclick: move |_| request_join(id, ActivityRole::Spectator),
                {crate::i18n::text("Watch", &[])}
            }
        }
    }
}

/// The chat header's list of what is running in the guild: a card can be
/// missed, or gone with a channel switch, and this stays until the game ends.
#[component]
pub fn OpenActivities() -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let mut open = use_signal(|| false);
    let unresolved = use_memo(move || {
        let s = state.read();
        let guild = s.selected_guild?;
        s.channels
            .iter()
            .filter(|c| c.guild_id == guild)
            .filter_map(|c| s.activity_instances.get(&c.id))
            .flatten()
            .any(|i| activity_of(&s, &i.app).is_none())
            .then_some(guild)
    });
    use_effect(move || {
        if let Some(guild_id) = unresolved() {
            gateway.send(ClientMessage::FetchGuildActivities { guild_id });
        }
    });

    let s = state.read();
    let running = open_in_guild(&s);
    let me = s
        .self_user
        .as_ref()
        .map(|u| u.pubkey.clone())
        .unwrap_or_default();
    let rows: Vec<_> = running
        .iter()
        .map(|(instance, activity)| {
            let channel = s
                .channels
                .iter()
                .find(|c| c.id == instance.channel_id)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            let host = instance
                .host()
                .map(|h| s.display_name(h))
                .unwrap_or_default();
            (instance.clone(), activity.clone(), channel, host)
        })
        .collect();
    drop(s);
    let count = rows.len();

    rsx! {
        if count > 0 {
            button {
                r#type: "button",
                class: if open() {
                    "h-8 px-2.5 flex items-center gap-2 shrink-0 rounded-lg border border-[var(--accent)] bg-[var(--panel2)] text-[11px] text-[var(--accent)] transition-colors"
                } else {
                    "h-8 px-2.5 flex items-center gap-2 shrink-0 rounded-lg border border-[var(--border)] bg-[var(--panel2)] text-[11px] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors"
                },
                title: crate::i18n::text("Activities running in this community", &[]),
                aria_pressed: open(),
                onclick: move |_| open.set(!open()),
                span { class: "w-2 h-2 rounded-full bg-[var(--accent)]" }
                {crate::i18n::text("Activities", &[])}
                span { class: "min-w-5 h-5 px-1.5 flex items-center justify-center rounded-md bg-[var(--accent-soft)] text-[10px] text-[var(--accent)]",
                    "{count}"
                }
            }
        }
        if open() {
            div { class: "fixed inset-0 z-20", onclick: move |_| open.set(false) }
            // Anchored to the chat pane like `ChatTools`' panels (trap 22).
            div {
                class: "dxf-pop-in absolute z-30 flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-xl shadow-lg text-xs text-[var(--text)]",
                style: "top: 3.25rem; right: 0.75rem; width: min(24rem, calc(100% - 1.5rem)); max-height: 70%;",
                div { class: "h-10 px-3 flex items-center justify-between border-b border-[var(--border)] shrink-0",
                    span { class: "text-[11px] uppercase tracking-wider text-[var(--text-dim)]", {crate::i18n::text("Open activities", &[])} }
                    button {
                        r#type: "button",
                        class: "w-7 h-7 flex items-center justify-center rounded-lg text-[var(--text-muted)] hover:text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                        title: crate::i18n::text("Close", &[]),
                        onclick: move |_| open.set(false),
                        span { class: "block w-4 h-4", dangerous_inner_html: super::icons::CLOSE }
                    }
                }
                div { class: "flex-1 min-h-0 overflow-y-auto p-2 flex flex-col",
                    if rows.is_empty() {
                        p { class: "py-4 text-center text-[var(--text-dim)]", {crate::i18n::text("Nothing is running right now.", &[])} }
                    }
                    for (instance, activity, channel, host) in rows {
                        OpenActivityRow { key: "{instance.id}", mine: instance.role_of(&me), instance, activity, channel, host,
                            on_pick: move |_| open.set(false) }
                    }
                }
            }
        }
    }
}

#[component]
fn OpenActivityRow(
    instance: ActivityInstance,
    activity: Activity,
    channel: String,
    host: String,
    mine: Option<ActivityRole>,
    on_pick: EventHandler<()>,
) -> Element {
    let players = instance.players();
    let watching = instance.participants.len() - players;
    let full = players >= instance.max_players as usize;
    let id = instance.id;
    let pick = move |role: ActivityRole| {
        request_join(id, role);
        on_pick.call(());
    };
    rsx! {
        div { class: "flex items-center gap-2.5 px-2 py-2 rounded hover:bg-white/[0.03]",
            span { class: "w-9 h-9 rounded-md border border-[var(--border)] flex items-center justify-center text-lg shrink-0",
                "{activity.icon}"
            }
            div { class: "flex-1 min-w-0",
                div { class: "text-sm text-[var(--text)] truncate", {activity.display_name()} }
                div { class: "text-[10px] text-[var(--text-dim)] truncate",
                    "# {channel} · "
                    {crate::i18n::text("Hosted by {0}", &[host.clone()])}
                }
                div { class: "text-[10px] text-[var(--text-dim)]",
                    {crate::i18n::text("{0}/{1} playing", &[players.to_string(), instance.max_players.to_string()])}
                    if watching > 0 { {crate::i18n::text(" · {0} watching", &[watching.to_string()])} }
                }
            }
            if let Some(role) = mine {
                button {
                    class: "shrink-0 rounded px-2 py-0.5 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                    onclick: move |_| pick(role),
                    {crate::i18n::text("Open", &[])}
                }
            } else {
                if !full {
                    button {
                        class: "shrink-0 rounded px-2 py-0.5 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                        onclick: move |_| pick(ActivityRole::Player),
                        {crate::i18n::text("Join", &[])}
                    }
                }
                button {
                    class: "shrink-0 rounded px-2 py-0.5 text-[10px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                    onclick: move |_| pick(ActivityRole::Spectator),
                    {crate::i18n::text("Watch", &[])}
                }
            }
        }
    }
}

#[component]
fn ActivityRow(
    activity: Activity,
    on_pick: EventHandler<Activity>,
    #[props(default = false)] disabled: bool,
) -> Element {
    let third_party = matches!(activity.source, Source::App { .. });
    let pick = activity.clone();
    rsx! {
        button {
            class: "w-full flex items-center gap-3 px-2 py-2 rounded hover:bg-white/[0.03] disabled:opacity-50 text-left transition-colors",
            disabled,
            onclick: move |_| on_pick.call(pick.clone()),
            span { class: "w-9 h-9 rounded-md border border-[var(--border)] flex items-center justify-center text-lg shrink-0",
                "{activity.icon}"
            }
            div { class: "flex-1 min-w-0",
                div { class: "text-sm text-[var(--text)] truncate", {activity.display_name()} }
                div { class: "text-[10px] text-[var(--text-dim)]",
                    if third_party { {crate::i18n::text("Installed app · ", &[])} } else { {crate::i18n::text("Built in · ", &[])} }
                    if activity.max_players > 1 { {crate::i18n::text("up to {0} players · ", &[(activity . max_players).to_string()])} }
                    {crate::i18n::plural("{0} permission", "{0} permissions", activity . caps . len ())}
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
                    div { class: "text-sm text-[var(--text)] font-medium truncate", {activity.display_name()} }
                    if let Some(author) = &author {
                        div { class: "font-mono text-[10px] text-[var(--text-dim)]", {crate::i18n::text("Third-party · {0}", std::slice::from_ref(author))} }
                    }
                }
            }
            div { class: "text-xs text-[var(--text-muted)] mb-1.5", {crate::i18n::text("This activity will be able to:", &[])} }
            ul { class: "space-y-1 mb-3",
                for cap in activity.caps.iter().copied() {
                    li { class: "flex items-start gap-2 text-xs text-[var(--text)]",
                        span { class: "text-[var(--accent)]", "•" }
                        span { {crate::i18n::label(cap.label())} }
                    }
                }
                if activity.caps.is_empty() {
                    li { class: "text-xs text-[var(--text-dim)]", {crate::i18n::text("Nothing beyond drawing in its window", &[])} }
                }
            }
            div { class: "text-[10px] text-[var(--text-dim)] mb-2",
                {crate::i18n::text("Opens in its own window with no internet access. It can't read your other channels, files, or keys, and it never needs your recovery phrase.", &[])}
            }
            div { class: "flex gap-2",
                button {
                    class: "flex-1 rounded px-2 py-1.5 text-[11px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                    onclick: move |_| on_approve.call(()),
                    if joining { {crate::i18n::text("Join", &[])} } else { {crate::i18n::text("Launch", &[])} }
                }
                button {
                    class: "rounded px-3 py-1.5 text-[11px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                    onclick: move |_| on_cancel.call(()),
                    {crate::i18n::text("Cancel", &[])}
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

const BUILTIN_MESSAGES: &[&str] = &[
    "Roll",
    "Share to chat",
    "Shared!",
    "Denied",
    "Hi, {0}",
    "shares go to #{0}",
    "Connecting…",
    "someone",
    "Waiting for a second player…",
    "Watching round {0}",
    "Round {0} — pick one",
    "{0} watching",
    "{0} didn't reveal — round to {1}",
    "draw",
    "{0} wins the round",
    "You picked {0} — waiting for {1}",
    "{0} has picked",
    "{0}'s move didn't match their commitment — round to {1}",
    "nobody",
    "{0} left — round to {1}",
    "This game has ended.",
];

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

    /* BUILTIN_TRANSLATIONS */
    const tr = (key, ...args) => (translations[key] || key).replace(/\{(\d+)\}/g, (token, i) => args[i] === undefined ? token : String(args[i]));
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
    rollBtn.textContent = tr('Roll');
    shareBtn.textContent = tr('Share to chat');
    const faces = ['⚀','⚁','⚂','⚃','⚄','⚅'];
    let last = 0;

    dxf.getUser().then(function (u) { whoEl.textContent = tr('Hi, {0}', u.username); })
       .catch(function () { whoEl.textContent = ''; });

    function showWhere() {
      dxf.getChannel()
         .then(function (c) { whereEl.textContent = c && c.name ? tr('shares go to #{0}', c.name) : ''; })
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
         .then(function () { showWhere(); shareBtn.textContent = tr('Shared!'); setTimeout(function(){ shareBtn.textContent=tr('Share to chat'); shareBtn.disabled=false; }, 1200); })
         .catch(function (err) { shareBtn.textContent = tr('Denied'); });
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

    /* BUILTIN_TRANSLATIONS */
    const tr = (key, ...args) => (translations[key] || key).replace(/\{(\d+)\}/g, (token, i) => args[i] === undefined ? token : String(args[i]));
    const FACE = { rock: '✊', paper: '✋', scissors: '✌️' };
    const BEATS = { rock: 'scissors', paper: 'rock', scissors: 'paper' };
    const REVEAL_WITHIN_MS = 15000;
    let info = null, round = 1, score = {}, mine = null;
    let committed = {}, revealed = {}, revealing = false, deadline = null, settled = false;
    const $ = (id) => document.getElementById(id);
    $('status').textContent = tr('Connecting…');

    function players() { return info ? info.participants.filter(p => p.role === 'player') : []; }
    function nameOf(k) { const p = info && info.participants.find(p => p.pubkey === k); return p ? p.name : tr('someone'); }
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
      else if (waiting) { $('status').textContent = tr('Waiting for a second player…'); }
      else if (!amPlayer()) { $('status').textContent = tr('Watching round {0}', round); }
      else if (!mine) { $('status').textContent = tr('Round {0} — pick one', round); }
      const watchers = info ? info.participants.length - ps.length : 0;
      $('who').textContent = watchers ? tr('{0} watching', watchers) : '';
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
        if (missing) award(other && other.pubkey, tr("{0} didn't reveal — round to {1}", nameOf(missing.pubkey), nameOf(other.pubkey)));
      }, REVEAL_WITHIN_MS);
      maybeResolve();
    }

    function maybeResolve() {
      const [a, b] = players();
      if (!a || !b || !revealed[a.pubkey] || !revealed[b.pubkey]) return;
      const pa = revealed[a.pubkey], pb = revealed[b.pubkey];
      const shown = FACE[pa] + ' vs ' + FACE[pb] + ' — ';
      if (pa === pb) award(null, shown + tr('draw'));
      else if (BEATS[pa] === pb) award(a.pubkey, shown + tr('{0} wins the round', nameOf(a.pubkey)));
      else award(b.pubkey, shown + tr('{0} wins the round', nameOf(b.pubkey)));
    }

    document.querySelectorAll('[data-pick]').forEach(b => b.addEventListener('click', () => {
      if (mine || !amPlayer()) return;
      mine = b.dataset.pick;
      render(tr('You picked {0} — waiting for {1}', FACE[mine], nameOf(opponent())));
      discordia.call('instance.commit', { round: String(round), value: mine })
        .then(() => { committed[info.me] = true; maybeReveal(); })
        .catch(() => { mine = null; render(); });
    }));

    discordia.on('instance.commit', (e) => {
      if (e.round !== String(round)) return;
      committed[e.from] = true;
      if (!amPlayer()) render(tr('{0} has picked', nameOf(e.from)));
      maybeReveal();
    });
    discordia.on('instance.reveal', (e) => {
      if (e.round !== String(round)) return;
      if (!e.valid || !FACE[e.value]) {
        const other = players().find(p => p.pubkey !== e.from);
        award(other && other.pubkey, tr("{0}'s move didn't match their commitment — round to {1}", nameOf(e.from), nameOf(other && other.pubkey)));
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
          award(stayer && stayer.pubkey, tr('{0} left — round to {1}', gone[0].name, stayer ? stayer.name : tr('nobody')));
          return;
        }
      }
      if (!settled) render();
    });

    discordia.call('instance.info').then((i) => { info = i; render(); })
      .catch((e) => { $('status').textContent = tr('This game has ended.'); });
  </script>
</body></html>"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_window_reopens_picker_and_leaves_shared_instance() {
        #[derive(Clone)]
        struct Props {
            tx: tokio::sync::mpsc::UnboundedSender<ClientMessage>,
            picker: std::rc::Rc<std::cell::Cell<Option<Signal<bool>>>>,
        }
        impl PartialEq for Props {
            fn eq(&self, _: &Self) -> bool {
                true
            }
        }
        fn harness(props: Props) -> Element {
            use_context_provider(|| GatewayTx(props.tx.clone()));
            use_context_provider(|| Signal::new(AppState::empty()));
            let picker = use_signal(|| false);
            use_hook(|| props.picker.set(Some(picker)));
            rsx! { ActivityHost { picker } }
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let picker = std::rc::Rc::new(std::cell::Cell::new(None));
        let mut dom = VirtualDom::new_with_props(
            harness,
            Props {
                tx,
                picker: picker.clone(),
            },
        );
        dom.rebuild_in_place();
        let id = Id::new_v4();
        let session = Session::new(Vec::new(), None, Some(id));
        let window = session.window;
        INBOX.with(|inbox| {
            let co = inbox.borrow().as_ref().copied().unwrap();
            co.send(HostCmd::Begin(session));
            co.send(HostCmd::Window(
                window,
                FromActivity::Failed("test open failure".into()),
            ));
        });
        for _ in 0..20 {
            if tokio::time::timeout(std::time::Duration::from_millis(20), dom.wait_for_work())
                .await
                .is_err()
            {
                break;
            }
            dom.render_immediate(&mut dioxus_core::NoOpMutations);
        }
        assert!(*picker.get().unwrap().peek());
        assert!(
            matches!(rx.try_recv(), Ok(ClientMessage::ActivityLeave { instance_id }) if instance_id == id)
        );
    }

    #[test]
    fn localized_builtin_bundles_have_distinct_cache_keys() {
        let mut hashes = std::collections::HashSet::new();
        for id in ["dice", "rps"] {
            for language in [
                crate::i18n::Language::English,
                crate::i18n::Language::Spanish,
                crate::i18n::Language::Portuguese,
            ] {
                let (hash, bundle) = builtin_bundle_in(id, language).unwrap();
                assert!(hashes.insert(hash));
                let html = std::str::from_utf8(&bundle.files["index.html"].bytes).unwrap();
                assert!(!html.contains("/* BUILTIN_TRANSLATIONS */"));
                assert!(html.contains(&format!(
                    "document.documentElement.lang = '{}'",
                    language.tag()
                )));
                assert!(html.contains(&crate::i18n::translate(language, "Share to chat", &[])));
            }
        }
    }

    #[test]
    fn activity_names_translate_only_for_builtins() {
        use std::{cell::RefCell, rc::Rc};
        type Names = Rc<RefCell<Vec<String>>>;
        fn probe() -> Element {
            let (language, names) = use_context::<(crate::i18n::Language, Names)>();
            let locale = use_signal(|| language);
            use_context_provider(|| crate::i18n::Locale(locale));
            let mut activity = Activity::from_builtin(&BUILTINS[0]);
            names.borrow_mut().push(activity.display_name().to_string());
            activity.source = Source::App {
                bot_pubkey: String::new(),
                sha256: String::new(),
            };
            names.borrow_mut().push(activity.display_name().to_string());
            rsx! {}
        }
        for (language, expected) in [
            (crate::i18n::Language::English, "Dice Roller"),
            (crate::i18n::Language::Spanish, "Dados"),
            (crate::i18n::Language::Portuguese, "Dados"),
        ] {
            let names: Names = Rc::new(RefCell::new(Vec::new()));
            VirtualDom::new(probe)
                .with_root_context((language, names.clone()))
                .rebuild_in_place();
            // Dioxus catches component panics, so assertions must run outside the render boundary.
            assert_eq!(*names.borrow(), [expected, "Dice Roller"]);
        }
    }

    #[tokio::test]
    async fn activity_watch_fetches_per_guild_not_per_reply_or_channel() {
        use std::{cell::RefCell, rc::Rc};

        type Export = Rc<RefCell<Option<Signal<AppState>>>>;
        let exported: Export = Rc::new(RefCell::new(None));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut dom = VirtualDom::new_with_props(
            |(export, gateway): (Export, GatewayTx)| {
                let state = use_signal(AppState::empty);
                *export.borrow_mut() = Some(state);
                use_activity_instance_watch(state, gateway);
                rsx! {}
            },
            (exported.clone(), GatewayTx(tx)),
        );
        dom.rebuild_in_place();
        dom.render_immediate_to_vec();
        assert!(rx.try_recv().is_err());
        let mut state = exported.borrow().unwrap();
        let guild = Id::new_v4();
        let channel = Id::new_v4();
        {
            let mut s = state.write();
            s.status = ConnectionStatus::Ready;
            s.selected_guild = Some(guild);
            s.selected_channel = Some(channel);
        }
        dom.render_immediate_to_vec();
        assert!(
            matches!(rx.try_recv().unwrap(), ClientMessage::FetchGuildActivityInstances { guild_id } if guild_id == guild)
        );
        for i in 0..400 {
            {
                let mut s = state.write();
                s.activity_instances.insert(channel, Vec::new());
                s.voice.speaking = i % 2 == 0;
            }
            dom.render_immediate_to_vec();
        }
        state.write().selected_channel = Some(Id::new_v4());
        dom.render_immediate_to_vec();
        assert!(rx.try_recv().is_err(), "pushes keep the guild current");
        state.write().status = ConnectionStatus::Reconnecting;
        dom.render_immediate_to_vec();
        assert!(rx.try_recv().is_err());
        state.write().status = ConnectionStatus::Ready;
        dom.render_immediate_to_vec();
        assert!(
            matches!(rx.try_recv().unwrap(), ClientMessage::FetchGuildActivityInstances { guild_id } if guild_id == guild)
        );
        let next = Id::new_v4();
        state.write().selected_guild = Some(next);
        dom.render_immediate_to_vec();
        assert!(
            matches!(rx.try_recv().unwrap(), ClientMessage::FetchGuildActivityInstances { guild_id } if guild_id == next)
        );
        state.write().dm_mode = true;
        dom.render_immediate_to_vec();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn open_in_guild_lists_every_channel_of_the_selected_guild_only() {
        use crate::protocol::{ActivityParticipant, Channel, ChannelKind};
        let (guild, other) = (Id::new_v4(), Id::new_v4());
        let channel = |guild_id: Id| Channel {
            id: Id::new_v4(),
            guild_id,
            name: "c".into(),
            kind: ChannelKind::Text,
            topic: None,
            read_only: false,
            slowmode_secs: 0,
            position: 0,
            access: None,
            divider: Default::default(),
            markdown: false,
        };
        let (a, b, c) = (channel(guild), channel(guild), channel(other));
        let instance = |channel_id: Id| ActivityInstance {
            id: Id::new_v4(),
            channel_id,
            app: "builtin:rps".into(),
            max_players: 2,
            participants: vec![ActivityParticipant {
                pubkey: "host".into(),
                role: ActivityRole::Player,
            }],
        };
        let mut s = AppState::empty();
        s.selected_guild = Some(guild);
        s.selected_channel = Some(a.id);
        for ch in [&a, &b, &c] {
            s.activity_instances.insert(ch.id, vec![instance(ch.id)]);
        }
        s.channels = vec![a.clone(), b.clone(), c];
        let mut found: Vec<Id> = open_in_guild(&s)
            .iter()
            .map(|(i, _)| i.channel_id)
            .collect();
        found.sort();
        let mut want = vec![a.id, b.id];
        want.sort();
        assert_eq!(found, want, "a game in another channel is still listed");
    }

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
    async fn unrelated_state_changes_do_not_refetch_the_guilds_activities() {
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
                s.status = ConnectionStatus::Ready;
                s.selected_guild = Some(Id::new_v4());
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
            if matches!(msg, ClientMessage::FetchGuildActivityInstances { .. }) {
                fetches += 1;
            }
        }
        assert_eq!(
            fetches, 1,
            "one fetch per guild selected, not one per state change"
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
