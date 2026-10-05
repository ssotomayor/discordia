use std::{cell::RefCell, collections::HashSet, rc::Rc};

use dioxus::prelude::*;
use tokio::sync::{mpsc, watch};

use crate::state::AppState;

#[derive(Clone, PartialEq)]
struct Stream {
    pubkey: String,
    name: String,
    volume: u32,
    muted: bool,
    has_audio: bool,
    viewers: Vec<String>,
}

#[derive(Clone, PartialEq)]
struct Snapshot {
    target: super::video_lifecycle::Target,
    key: Option<String>,
    encrypted: bool,
    theme: String,
    text_size_percent: u16,
    streams: Vec<Stream>,
}

enum Command {
    Dock(String),
    Stop(String),
    Volume(String, u32),
    Mute(String, bool),
    DockAll,
}

#[derive(Clone, Copy)]
pub(super) struct Popouts {
    pub detached: Signal<HashSet<String>>,
    pub open: EventHandler<String>,
}

pub(super) fn use_popouts(mut state: Signal<AppState>) -> Popouts {
    let gateway = crate::state::use_gateway();
    let mut reported = use_signal(|| None);
    use_effect(move || {
        let s = state.read();
        let mut sharers: Vec<_> = s.screen_viewing.iter().cloned().collect();
        sharers.sort();
        let next = (s.voice_session_epoch, s.voice.channel_id, sharers);
        if reported.peek().as_ref() != Some(&next) {
            if next.1.is_some() {
                gateway.send(crate::protocol::ClientMessage::SetScreenWatching {
                    sharers: next.2.clone(),
                });
            }
            reported.set(Some(next));
        }
    });
    let mut detached = use_signal(HashSet::<String>::new);
    let mut detached_epoch = use_signal(move || state.peek().voice_session_epoch);
    use_effect(move || {
        let s = state.read();
        let epoch = s.voice_session_epoch;
        let same_session = epoch == *detached_epoch.peek() && s.screen_viewer_token.is_some();
        let mut current = detached.read().clone();
        reconcile_detached(&mut current, &s.screen_viewing, same_session);
        if current != *detached.peek() {
            detached.set(current);
        }
        if epoch != *detached_epoch.peek() {
            detached_epoch.set(epoch);
        }
    });
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let mut generation = use_signal(|| 0_u64);
    let mut creating = use_signal(|| false);
    let handle = use_hook(|| Rc::new(RefCell::new(None::<dioxus::desktop::WeakDesktopContext>)));
    let bridge = use_hook(|| {
        let (snapshots, snapshot_rx) = watch::channel(None::<Snapshot>);
        let (commands, command_rx) = mpsc::unbounded_channel::<(u64, Command)>();
        (
            snapshots,
            snapshot_rx,
            commands,
            Rc::new(RefCell::new(Some(command_rx))),
        )
    });
    let snapshot = use_memo(move || {
        let s = state.read();
        let detached = detached.read();
        let (url, token) = s.screen_viewer_token.as_ref()?;
        let mut streams: Vec<_> = detached
            .iter()
            .filter(|pk| s.screen_viewing.contains(*pk))
            .map(|pk| Stream {
                pubkey: pk.clone(),
                name: s.display_name(pk),
                volume: s.stream_volumes.get(pk).copied().unwrap_or(100),
                muted: s.stream_muted.contains(pk),
                has_audio: s.stream_has_audio.contains(pk),
                viewers: s.screen_viewer_names(pk),
            })
            .collect();
        streams.sort_by(|a, b| a.pubkey.cmp(&b.pubkey));
        if streams.is_empty() {
            return None;
        }
        Some(Snapshot {
            target: super::video_lifecycle::Target {
                url: url.clone(),
                token: token.clone(),
                voice_epoch: s.voice_session_epoch,
            },
            key: crate::e2ee::current_key(),
            encrypted: crate::e2ee::enabled(),
            theme: settings.read().theme.clone(),
            text_size_percent: settings.read().text_size_percent,
            streams,
        })
    });
    let sender = bridge.0.clone();
    let receiver = bridge.1.clone();
    let commands = bridge.2.clone();
    let window = dioxus::desktop::use_window();
    let handle_for_open = handle.clone();
    use_effect(move || {
        let next = snapshot();
        sender.send_replace(next.clone());
        let identities: Vec<_> = next
            .as_ref()
            .map(|s| s.streams.iter().map(|v| &v.pubkey).collect())
            .unwrap_or_default();
        let identities = serde_json::to_string(&identities).unwrap_or_else(|_| "[]".into());
        evaluate(&format!(
            "window.dxScreen.setDetachedScreens({identities});"
        ));
        if next.is_none() {
            close_popout(&handle_for_open);
            return;
        }
        if *creating.peek()
            || handle_for_open
                .borrow()
                .as_ref()
                .and_then(|h| h.upgrade())
                .is_some()
        {
            return;
        }
        creating.set(true);
        let id = *generation.peek() + 1;
        generation.set(id);
        let dom = VirtualDom::new_with_props(
            PopoutWindow,
            PopoutWindowProps {
                receiver: receiver.clone(),
                commands: commands.clone(),
                generation: id,
            },
        );
        let cfg = dioxus::desktop::Config::new()
            .with_window(
                dioxus::desktop::tao::window::WindowBuilder::new()
                    .with_title("Discordia — Streams")
                    .with_window_icon(crate::load_window_icon())
                    .with_inner_size(dioxus::desktop::tao::dpi::LogicalSize::new(1100.0, 700.0))
                    .with_min_inner_size(dioxus::desktop::tao::dpi::LogicalSize::new(480.0, 320.0))
                    .with_always_on_top(false),
            )
            .with_close_behaviour(dioxus::desktop::WindowCloseBehaviour::WindowCloses)
            .with_menu(None);
        let pending = window.new_window(dom, cfg);
        let handle = handle_for_open.clone();
        spawn(async move {
            let popup = pending.await;
            if snapshot.peek().is_none() {
                popup.close();
            } else {
                *handle.borrow_mut() = Some(Rc::downgrade(&popup));
            }
            creating.set(false);
        });
    });
    let command_rx = bridge.3.clone();
    let command_handle = handle.clone();
    use_future(move || {
        let rx = command_rx.borrow_mut().take();
        let handle = command_handle.clone();
        async move {
            let Some(mut rx) = rx else {
                return;
            };
            while let Some((id, command)) = rx.recv().await {
                if id != *generation.peek() {
                    continue;
                }
                match command {
                    Command::Dock(pk) => {
                        detached.write().remove(&pk);
                        if detached.peek().is_empty() {
                            close_popout(&handle);
                        }
                    }
                    Command::Stop(pk) => {
                        detached.write().remove(&pk);
                        state.write().screen_viewing.remove(&pk);
                        if detached.peek().is_empty() {
                            close_popout(&handle);
                        }
                    }
                    Command::Volume(pk, value) => {
                        state.write().stream_volumes.insert(pk, value);
                    }
                    Command::Mute(pk, muted) => {
                        if muted {
                            state.write().stream_muted.insert(pk);
                        } else {
                            state.write().stream_muted.remove(&pk);
                        }
                    }
                    Command::DockAll => {
                        close_popout(&handle);
                        detached.write().clear();
                    }
                }
            }
        }
    });
    let handle_for_drop = handle.clone();
    use_drop(move || {
        if let Some(popup) = handle_for_drop.borrow().as_ref().and_then(|h| h.upgrade()) {
            popup.close();
        }
    });
    Popouts {
        detached,
        open: EventHandler::new(move |pk| {
            if state.read().screen_viewer_token.is_none() {
                state.write().error_toast = Some("This server does not provide external stream windows yet. Update the server and rejoin voice.".into());
                return;
            }
            detached.write().insert(pk);
            if let Some(popup) = handle.borrow().as_ref().and_then(|h| h.upgrade()) {
                popup.set_visible(true);
                popup.window.set_focus();
            }
        }),
    }
}

fn reconcile_detached(
    detached: &mut HashSet<String>,
    viewing: &HashSet<String>,
    same_session: bool,
) {
    if same_session {
        detached.retain(|pk| viewing.contains(pk));
    } else {
        detached.clear();
    }
}

pub(super) fn grid_style(count: usize, width: f64, height: f64) -> String {
    let count = count.max(1);
    let mut best = (1, count, 0.0_f64);
    for cols in 1..=count.min(4) {
        let rows = count.div_ceil(cols);
        let tile_w = (width - 8.0 * (cols - 1) as f64) / cols as f64;
        let tile_h = ((height - 8.0 * (rows - 1) as f64) / rows as f64 - 82.0).max(0.0);
        let video_w = tile_w.min(tile_h * 16.0 / 9.0);
        let area = video_w * video_w * 9.0 / 16.0;
        if area > best.2 {
            best = (cols, rows, area);
        }
    }
    format!(
        "grid-template-columns:repeat({},minmax(0,1fr));grid-template-rows:repeat({},minmax(0,1fr));",
        best.0, best.1
    )
}

#[derive(Clone, Props)]
struct PopoutWindowProps {
    receiver: watch::Receiver<Option<Snapshot>>,
    commands: mpsc::UnboundedSender<(u64, Command)>,
    generation: u64,
}

impl PartialEq for PopoutWindowProps {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation
    }
}

#[allow(non_snake_case)]
fn PopoutWindow(props: PopoutWindowProps) -> Element {
    let mut model = use_signal(|| props.receiver.borrow().clone());
    let mut focused = use_signal::<Option<String>>(|| None);
    let pinned = use_signal(|| false);
    let mut status = use_signal(|| "Connecting…".to_owned());
    let mut active_generation = use_signal(|| 0_u64);
    let window = dioxus::desktop::use_window();
    let initial_size = window.window.inner_size();
    let mut size = use_signal(move || (initial_size.width as f64, initial_size.height as f64));
    let window_id = window.window.id();
    let event_window = window.clone();
    dioxus::desktop::use_wry_event_handler(move |event, _| {
        use dioxus::desktop::tao::event::{ElementState, Event, WindowEvent};
        if let Event::WindowEvent {
            window_id: id,
            event,
            ..
        } = event
            && *id == window_id
        {
            match event {
                WindowEvent::Resized(value) => {
                    size.set((value.width as f64, value.height as f64));
                }
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed
                        && event.logical_key == dioxus::desktop::tao::keyboard::Key::Escape =>
                {
                    focused.set(None);
                    event_window.window.set_fullscreen(None);
                }
                _ => {}
            }
        }
    });
    let receiver = props.receiver.clone();
    let close_window = window.clone();
    use_future(move || {
        let mut receiver = receiver.clone();
        let window = close_window.clone();
        async move {
            while receiver.changed().await.is_ok() {
                let next = receiver.borrow_and_update().clone();
                let done = next.is_none();
                model.set(next);
                if done {
                    window.close();
                    return;
                }
            }
            window.close();
        }
    });
    let lifecycle = use_hook(|| {
        let (targets, rx) = watch::channel(None::<super::video_lifecycle::Target>);
        let (events, event_rx) = mpsc::unbounded_channel::<super::video_lifecycle::Event>();
        (targets, rx, events, Rc::new(RefCell::new(Some(event_rx))))
    });
    let targets = lifecycle.0.clone();
    use_effect(move || {
        targets.send_replace(model.read().as_ref().map(|s| s.target.clone()));
    });
    let receiver = lifecycle.1.clone();
    let events = lifecycle.3.clone();
    use_future(move || {
        let receiver = receiver.clone();
        let events = events.borrow_mut().take();
        async move {
            let Some(events) = events else {
                return;
            };
            if !wait_for_bridge().await {
                status.set("The video viewer could not initialize.".into());
                return;
            }
            super::video_lifecycle::run(receiver, events, move |action| {
                let js = match action {
                    super::video_lifecycle::Action::Connect { target, generation } => {
                        active_generation.set(generation);
                        let Some(s) = model.peek().clone() else { return; };
                        let ids: Vec<_> = s.streams.iter().map(|v| &v.pubkey).collect();
                        format!("window.dxScreen.setNativeStreamAudio(true);window.dxScreen.setViewerTargets({});window.dxScreen.connect({},{},{},{},{});",
                            json(&ids), json(&target.url), json(&target.token), json(&s.key), s.encrypted, generation)
                    }
                    super::video_lifecycle::Action::Disconnect => "window.dxScreen.disconnect();".into(),
                };
                evaluate(&js);
            }).await;
        }
    });
    use_effect(move || {
        if let Some(s) = model.read().as_ref() {
            let ids: Vec<_> = s.streams.iter().map(|v| &v.pubkey).collect();
            evaluate(&format!(
                "window.dxScreen.setViewerTargets({});window.dxScreen.setE2eeKey({});",
                json(&ids),
                json(&s.key)
            ));
            if focused.peek().is_some()
                && (ids.len() <= 1 || focused.peek().as_ref().is_some_and(|pk| !ids.contains(&pk)))
            {
                focused.set(None);
            }
        }
    });
    let events = lifecycle.2.clone();
    use_future(move || {
        let events = events.clone();
        async move {
            let mut eval = document::eval(
                r#"
              window.addEventListener('message', function(e) {
                const d = e.data;
                if (d && (d.__dxf === 'screen-room-state' || d.__dxf === 'e2ee-error' || d.__dxf === 'screen-room-error')) dioxus.send(d);
              });
            "#,
            );
            while let Ok(value) = eval.recv::<serde_json::Value>().await {
                if value["__dxf"] == "screen-room-state" {
                    if value["generation"].as_u64() != Some(*active_generation.peek()) {
                        continue;
                    }
                    if let Ok(event) =
                        serde_json::from_value::<super::video_lifecycle::Event>(value.clone())
                        && events.send(event).is_err()
                    {
                        return;
                    }
                    status.set(
                        match value["status"].as_str() {
                            Some("connected") => "",
                            Some("blocked") => "Encryption unavailable",
                            _ => "Reconnecting…",
                        }
                        .into(),
                    );
                } else {
                    status.set(
                        value["detail"]
                            .as_str()
                            .unwrap_or("Could not connect")
                            .to_owned(),
                    );
                }
            }
        }
    });
    let commands = props.commands.clone();
    let generation = props.generation;
    use_drop(move || {
        evaluate("window.dxScreen.disconnect();");
        if commands.send((generation, Command::DockAll)).is_err() {
            tracing::debug!("stream window owner has gone away");
        }
    });
    let Some(snapshot) = model() else {
        return rsx! { crate::app::AppHead {} };
    };
    let theme = crate::app::theme_vars(&snapshot.theme);
    let size_css = crate::ui_size::text_css(snapshot.text_size_percent);
    let visible_count = if focused().is_some() {
        1
    } else {
        snapshot.streams.len()
    };
    let (width, height) = size();
    let scale = window.window.scale_factor();
    let grid = grid_style(visible_count, width / scale - 16.0, height / scale - 60.0);
    let multiple_streams = snapshot.streams.len() > 1;
    rsx! {
        crate::app::AppHead {}
        style { "{size_css}" }
        div { class: "dxf-ui h-full flex flex-col bg-[var(--bg)] text-[var(--text)]", style: "{theme}",
            if multiple_streams || !status().is_empty() {
                div { class: "flex items-center flex-wrap gap-2 px-3 py-2 border-b border-[var(--border)] shrink-0",
                    span { class: "flex-1 text-xs text-[var(--text-muted)]", "{status}" }
                    if multiple_streams {
                        WindowControls { pinned, commands: props.commands.clone(), generation }
                    }
                }
            }
            div { class: "flex-1 min-h-0 grid gap-2 p-2", style: "{grid}",
                for stream in snapshot.streams {
                    PopoutTile { key: "{stream.pubkey}", stream, focused, pinned, commands: props.commands.clone(), generation, multiple_streams }
                }
            }
        }
    }
}

#[derive(Clone, Props)]
struct WindowControlsProps {
    pinned: Signal<bool>,
    commands: mpsc::UnboundedSender<(u64, Command)>,
    generation: u64,
}

impl PartialEq for WindowControlsProps {
    fn eq(&self, other: &Self) -> bool {
        self.pinned == other.pinned && self.generation == other.generation
    }
}

#[allow(non_snake_case)]
fn WindowControls(props: WindowControlsProps) -> Element {
    let mut pinned = props.pinned;
    let commands = props.commands;
    let generation = props.generation;
    let pin_window = dioxus::desktop::use_window();
    let fullscreen_window = pin_window.clone();
    rsx! {
        div { class: "flex flex-wrap items-center justify-end gap-2",
            button { class: "px-2 py-1 rounded border border-[var(--border)] text-xs", title: "Keep window on top", aria_pressed: "{pinned}",
                onclick: move |_| { pinned.toggle(); pin_window.window.set_always_on_top(pinned()); },
                if pinned() { "Unpin" } else { "Pin" }
            }
            button { class: "px-2 py-1 rounded border border-[var(--border)] text-xs", onclick: move |_| {
                let active = fullscreen_window.window.fullscreen().is_some();
                fullscreen_window.window.set_fullscreen(if active { None } else { Some(dioxus::desktop::tao::window::Fullscreen::Borderless(None)) });
            }, "Full screen" }
            button { class: "px-2 py-1 rounded border border-[var(--border)] text-xs", onclick: move |_| send(&commands, generation, Command::DockAll), "Return to app" }
        }
    }
}

fn json(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".into())
}

fn close_popout(handle: &RefCell<Option<dioxus::desktop::WeakDesktopContext>>) {
    let previous = handle.borrow_mut().take();
    if let Some(window) = previous.and_then(|h| h.upgrade()) {
        window.close();
    }
}

fn evaluate(js: &str) {
    let _eval = document::eval(&format!("if (window.dxScreen) {{ {js} }}"));
}

async fn wait_for_bridge() -> bool {
    let mut eval = document::eval(
        r#"
      for (let n = 0; n < 200 && !window.dxScreen; n++) await new Promise(r => setTimeout(r, 50));
      dioxus.send(!!window.dxScreen);
    "#,
    );
    eval.recv::<bool>().await.unwrap_or(false)
}

fn send(commands: &mpsc::UnboundedSender<(u64, Command)>, generation: u64, command: Command) {
    if commands.send((generation, command)).is_err() {
        tracing::debug!("stream window owner has gone away");
    }
}

#[derive(Clone, Props)]
struct PopoutTileProps {
    stream: Stream,
    focused: Signal<Option<String>>,
    pinned: Signal<bool>,
    commands: mpsc::UnboundedSender<(u64, Command)>,
    generation: u64,
    multiple_streams: bool,
}

impl PartialEq for PopoutTileProps {
    fn eq(&self, other: &Self) -> bool {
        self.stream == other.stream
            && self.focused == other.focused
            && self.pinned == other.pinned
            && self.generation == other.generation
            && self.multiple_streams == other.multiple_streams
    }
}

#[allow(non_snake_case)]
fn PopoutTile(props: PopoutTileProps) -> Element {
    let stream = props.stream;
    let mut focused = props.focused;
    let can_focus = props.multiple_streams;
    let selected = can_focus && focused().as_ref() == Some(&stream.pubkey);
    let hidden = can_focus && focused().is_some() && !selected;
    let container = format!("popout-stream-{}", stream.pubkey);
    let pk = stream.pubkey.clone();
    let target = container.clone();
    use_future(move || {
        let pk = pk.clone();
        let target = target.clone();
        async move {
            if wait_for_bridge().await {
                evaluate(&super::screenshare::attach_js(&pk, &target, "screen"));
            }
        }
    });
    let detach = container.clone();
    use_drop(move || evaluate(&super::screenshare::detach_js(&detach)));
    let mut received = use_signal(|| "Waiting for stream…".to_owned());
    let mut quality_reason = use_signal(String::new);
    let pk = stream.pubkey.clone();
    let window = dioxus::desktop::use_window();
    use_future(move || {
        let pk = pk.clone();
        let window = window.clone();
        async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let visible = !window.window.is_minimized() && window.window.is_visible();
                let _ =
                    document::eval(&format!("window.dxScreen?.setViewerVisibility({visible});"));
                if !visible {
                    continue;
                }
                let mut eval = document::eval(&format!(
                    "dioxus.send(await window.dxScreen.previewStats({}));",
                    json(&pk)
                ));
                if let Ok(value) = eval.recv::<serde_json::Value>().await {
                    quality_reason.set(
                        value["qualityReason"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    );
                    if let (Some(w), Some(h), Some(fps)) = (
                        value["width"].as_u64(),
                        value["height"].as_u64(),
                        value["fps"].as_f64(),
                    ) {
                        let label = format!("{w}×{h} · {fps:.0} FPS");
                        if label != *received.peek() {
                            received.set(label);
                        }
                    }
                }
            }
        }
    });
    let pk_focus = stream.pubkey.clone();
    let pk_focus_key = stream.pubkey.clone();
    let pk_dock = stream.pubkey.clone();
    let pk_stop = stream.pubkey.clone();
    let pk_volume = stream.pubkey.clone();
    let pk_mute = stream.pubkey.clone();
    let dock = props.commands.clone();
    let stop = props.commands.clone();
    let volume = props.commands.clone();
    let mute = props.commands.clone();
    let generation = props.generation;
    rsx! {
        div { class: "min-w-0 min-h-0 flex flex-col border border-[var(--border)] rounded-lg overflow-hidden bg-[var(--panel-solid)]", style: if hidden { "display:none;" } else { "display:flex;" },
            div { class: "flex flex-wrap items-center gap-2 px-3 py-2 shrink-0",
                span { class: "flex-1 min-w-0 truncate text-sm font-medium", "{stream.name}" }
                super::screenshare::StreamViewers { names: stream.viewers.clone() }
                if props.multiple_streams {
                    button { class: "text-xs px-2 py-1 rounded hover:bg-[var(--panel2)]", onclick: move |_| send(&dock, generation, Command::Dock(pk_dock.clone())), "Return" }
                }
                button { class: "text-xs px-2 py-1 rounded hover:bg-[var(--panel2)]", aria_label: "Stop watching", onclick: move |_| send(&stop, generation, Command::Stop(pk_stop.clone())), "✕" }
            }
            div { id: "{container}", class: "flex-1 min-h-0 bg-black flex items-center justify-center text-sm text-[var(--text-muted)]",
                style: if can_focus { "cursor: pointer;" } else { "cursor: default;" },
                role: if can_focus { "button" } else { "group" },
                tabindex: if can_focus { "0" } else { "-1" },
                title: can_focus.then_some(if selected { "Return to mosaic" } else { "Focus stream" }),
                aria_label: if can_focus { if selected { "Return to mosaic" } else { "Focus stream" }.to_owned() } else { format!("{}'s screen", stream.name) },
                aria_pressed: can_focus.then(|| selected.to_string()),
                onclick: move |e| {
                    e.stop_propagation();
                    if can_focus {
                        focused.set(if selected { None } else { Some(pk_focus.clone()) });
                    }
                },
                onkeydown: move |e| {
                    if can_focus && (e.key() == Key::Enter || e.key() == Key::Character(" ".into())) {
                        e.prevent_default();
                        e.stop_propagation();
                        focused.set(if selected { None } else { Some(pk_focus_key.clone()) });
                    }
                },
                "Connecting to stream…"
            }
            div { class: "grid items-center gap-2 px-3 py-2 shrink-0", style: "grid-template-columns:minmax(0,1fr) auto minmax(0,1fr);",
                div { class: "min-w-0",
                    span { class: "text-xs text-[var(--text-muted)]", "{received}" }
                    super::screenshare::StreamQualityNotice { reason: quality_reason() }
                }
                div { class: "flex items-center gap-2",
                button { class: "text-xs px-2 py-1 rounded border border-[var(--border)]", disabled: !stream.has_audio, aria_pressed: "{stream.muted}", onclick: move |_| send(&mute, generation, Command::Mute(pk_mute.clone(), !stream.muted)), if stream.muted { "Unmute" } else { "Mute" } }
                input { r#type: "range", min: "0", max: "100", value: "{stream.volume}", class: "w-24 accent-[var(--accent)]", aria_label: "Stream volume", disabled: !stream.has_audio || stream.muted,
                    oninput: move |event| if let Ok(value) = event.value().parse::<u32>() { send(&volume, generation, Command::Volume(pk_volume.clone(), value.min(100))); }
                }
                span { class: "text-xs text-[var(--text-muted)]", "{stream.volume}%" }
                }
                div { class: "min-w-0 justify-self-end",
                    if !props.multiple_streams {
                        WindowControls { pinned: props.pinned, commands: props.commands.clone(), generation }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{grid_style, reconcile_detached};
    use std::collections::HashSet;

    #[test]
    fn a_rewatched_stream_stays_in_the_app_until_explicitly_detached_again() {
        let mut detached = HashSet::from(["A".into(), "B".into()]);
        let mut viewing = detached.clone();
        viewing.remove("A");
        reconcile_detached(&mut detached, &viewing, true);
        assert_eq!(detached, HashSet::from(["B".into()]));
        viewing.insert("A".into());
        reconcile_detached(&mut detached, &viewing, true);
        assert!(!detached.contains("A"));
        assert!(detached.contains("B"));
        detached.insert("A".into());
        reconcile_detached(&mut detached, &viewing, true);
        assert!(detached.contains("A"));
    }

    #[test]
    fn changing_voice_sessions_drops_old_external_window_selections() {
        let viewing = HashSet::from(["A".into(), "B".into()]);
        let mut detached = viewing.clone();
        reconcile_detached(&mut detached, &viewing, false);
        assert!(detached.is_empty());
        reconcile_detached(&mut detached, &viewing, true);
        assert!(detached.is_empty());
    }

    #[test]
    fn two_streams_fit_side_by_side_on_a_wide_window() {
        assert!(grid_style(2, 1500.0, 650.0).contains("columns:repeat(2,"));
    }

    #[test]
    fn a_narrow_window_stacks_two_streams() {
        assert!(grid_style(2, 500.0, 1000.0).contains("columns:repeat(1,"));
    }

    #[test]
    fn four_streams_use_a_two_by_two_mosaic() {
        let layout = grid_style(4, 1200.0, 800.0);
        assert!(layout.contains("columns:repeat(2,"));
        assert!(layout.contains("rows:repeat(2,"));
    }
}
