use dioxus::prelude::*;
use dioxus_grid_layout::{
    FloatRect, GridItem, GridLayout, GridPosition, LayoutMode, use_layout_store,
};

use crate::features::{
    channels::ChannelsColumn, chat::ChatView, guilds::GuildsSidebar, members::MembersPanel,
    voice::spawn_voice_service,
};
use crate::identity::Identity;
use crate::net::spawn_gateway;
use crate::protocol::{ClientMessage, Id};
use crate::state::{
    AppState, ConnectionStatus, HomeView, SessionMode, SessionParams, VoicePhase, use_app_state,
    use_gateway,
};

/// Vertical row span each panel occupies, and the gap (px) between grid
/// rows. The on-mount measurement divides the available height by these so
/// the panels fill the viewport without scrolling.
const GRID_ROWS: u32 = 30;
const GRID_GAP: f64 = 8.0;

/// Write the current arrangement to local settings. Called after every
/// drag/resize commit and on reset — previously nothing persisted the layout at
/// all, so every launch started from the default.
fn persist_layout(
    mut settings: Signal<crate::settings::ClientSettings>,
    layout: dioxus_grid_layout::LayoutStore,
) {
    let mut next = settings.read().clone();
    next.layout_cells = layout
        .snapshot()
        .into_iter()
        .map(|(id, p)| (id, [p.x, p.y, p.w, p.h]))
        .collect();
    next.layout_free = layout
        .free_snapshot()
        .into_iter()
        .map(|(id, r)| (id, [r.x, r.y, r.w, r.h]))
        .collect();
    settings.set(next.clone());
    crate::settings::save(&next);
}

/// The stock four-column dashboard, used on first launch and by "Reset".
fn default_layout() -> Vec<(String, GridPosition)> {
    vec![
        ("guilds".into(), GridPosition::new(0, 0, 1, GRID_ROWS)),
        ("channels".into(), GridPosition::new(1, 0, 2, GRID_ROWS)),
        ("chat".into(), GridPosition::new(3, 0, 7, GRID_ROWS)),
        ("members".into(), GridPosition::new(10, 0, 2, GRID_ROWS)),
    ]
}

/// Power / unplug glyph for the disconnect button. Inherits `currentColor`
/// so it picks up the button's text colour (and the danger hover state).
const UNPLUG_ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18.36 6.64a9 9 0 1 1-12.73 0"/><line x1="12" y1="2" x2="12" y2="12"/></svg>"##;

/// The whole app past identity setup, with or without a gateway.
///
/// `params` is optional because home does not need a server: `Offline` is a
/// first-class state here, not a failure. Everything session-shaped — the
/// guild rail's contents, the members panel, voice — is simply empty in it,
/// while the Nostr half (direct messages, contacts) runs exactly the same,
/// because it never depended on the gateway in the first place.
#[component]
pub fn WorkspaceView(
    params: Option<SessionParams>,
    identity: Identity,
    on_disconnect: EventHandler<String>,
    on_switch: EventHandler<SessionMode>,
    /// Why the last session ended, when it ended badly. The connect screen
    /// used to be where this landed; without it, a dropped connection would
    /// otherwise return you to home with no account of itself.
    notice: Option<String>,
    /// Identity edits, owned by `App` because the identity outlives any
    /// session, and rendered by home's column footer.
    on_rename: EventHandler<String>,
    on_sign_out: EventHandler<()>,
) -> Element {
    let state = use_signal(AppState::empty);
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();

    let (gateway_tx, voice_tx, nostr_tx) = use_hook(|| {
        // Must restore persisted audio prefs before the voice service starts,
        // as it seeds live controls from AppState on first poll.
        {
            let saved = settings.read();
            let mut app = state;
            let mut w = app.write();
            w.mic_sensitivity = saved.mic_sensitivity.clamp(1, 1000);
            w.mic_volume = saved.mic_volume.min(200);
            w.auto_gain_control = saved.auto_gain_control;
            w.noise_cancellation = saved.noise_cancellation;
            // Only valid where raw capture is supported; prevents a Windows
            // settings file from leaving a macOS session believing it captures
            // raw.
            w.bypass_system_audio_processing =
                saved.bypass_system_audio_processing && crate::rawmic::supported();
            // Clamps hand-edited values to the slider's domain; unlike
            // mic_sensitivity, this is bound directly to the dB value.
            w.denoise_atten_lim_db = saved.denoise_atten_lim_db.clamp(
                crate::features::voice::DENOISE_ATTEN_LIM_DB_MIN,
                crate::features::voice::DENOISE_ATTEN_LIM_DB_MAX,
            );
            // Values outside the offered set indicate a hand-edited
            // settings.json; fall back to a valid bitrate.
            w.voice_bitrate_kbps = match saved.voice_bitrate_kbps {
                24 => 24,
                _ => 48,
            };
            w.selected_input_device = saved.selected_input_device.clone();
            w.selected_output_device = saved.selected_output_device.clone();
        }
        let voice_tx = spawn_voice_service(state);
        // Offline still needs a `GatewayTx` in context, because every panel
        // asks for one. Its receiver is dropped with this block, so a stray
        // send fails silently — which is why the panels that *could* send are
        // gated on `status` rather than left to discover it. Nothing here
        // routes a message to a server that was never dialled.
        let gateway_tx = match params.clone() {
            Some(p) => {
                let mut app = state;
                app.write().server_label = Some(p.mode.label());
                spawn_gateway(p, state, voice_tx.clone(), move |reason| {
                    on_disconnect.call(reason);
                })
            }
            None => {
                let mut app = state;
                let mut w = app.write();
                w.status = ConnectionStatus::Offline;
                // There is nothing else to be looking at: no guild has been
                // loaded, and none can be.
                w.dm_mode = true;
                drop(w);
                let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                crate::state::GatewayTx(tx)
            }
        };
        // DMs use Nostr relays, not the gateway, so this runs independently of
        // gateway status.
        let relays = {
            let saved = settings.read();
            if saved.dm_relays.is_empty() {
                crate::nostr::relay::DEFAULT_RELAYS
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            } else {
                saved.dm_relays.clone()
            }
        };
        let nostr_tx = crate::nostr::service::spawn_nostr(identity.clone(), relays, state);
        (gateway_tx, voice_tx, nostr_tx)
    });
    provide_context(crate::features::home::IdentityActions {
        on_rename,
        on_sign_out,
    });
    provide_context(gateway_tx.clone());
    provide_context(nostr_tx.clone());
    provide_context(crate::features::voice::VoiceTx(voice_tx.clone()));
    provide_context(state);
    // The Nostr identity (with signing key) — used to authorize Blossom uploads.
    provide_context(identity.clone());

    // Initial 4-panel dashboard layout. 12 cols, each panel spans the full
    // GRID_ROWS height so the four columns sit side by side. The pixel row
    // height is derived at mount from the available viewport (see below) so
    // the panels exactly fill the window on first open instead of overflowing
    // into a scroll. Users can still drag/resize via the corner grip.
    let mut layout = use_layout_store(|| {
        let saved = settings.read();
        if saved.layout_cells.is_empty() {
            return default_layout();
        }
        saved
            .layout_cells
            .iter()
            .map(|(id, [x, y, w, h])| (id.clone(), GridPosition::new(*x, *y, *w, *h)))
            .collect()
    });
    use_hook(|| {
        let saved = settings.read();
        if saved.layout_free.is_empty() {
            return;
        }
        let mut store = layout;
        for (id, [x, y, w, h]) in &saved.layout_free {
            store.set_free(id.clone(), FloatRect::new(*x, *y, *w, *h));
        }
    });

    let mut edit_mode = use_signal(|| false);
    let status = state.read().status;
    let home_pane = {
        let s = state.read();
        s.dm_mode.then_some(s.home_view)
    };
    // Home with no conversation open has nothing to draw a chat for, and an
    // empty composer is the least informative thing to show somebody on a
    // first launch.
    let home_empty = {
        let s = state.read();
        s.dm_mode && s.selected_channel.is_none()
    };

    // Guild accent overrides app-level accent inline; suppressed in DM mode.
    let guild_accent_style = {
        let s = state.read();
        let accent = if s.dm_mode {
            None
        } else {
            s.selected_guild.and_then(|gid| {
                s.guilds
                    .iter()
                    .find(|g| g.id == gid)
                    .and_then(|g| g.accent.clone())
            })
        };
        accent
            .map(|a| crate::app::accent_vars(&a))
            .unwrap_or_default()
    };

    // Prune stale typing indicators (>5s) to avoid idle re-renders.
    use_future(move || async move {
        let mut state = state;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            let now = std::time::Instant::now();
            let stale = {
                let s = state.read();
                s.typing.values().any(|set| {
                    set.values()
                        .any(|(_, t)| now.duration_since(*t).as_secs() >= 5)
                })
            };
            if stale {
                let mut s = state.write();
                for set in s.typing.values_mut() {
                    set.retain(|_, (_, t)| now.duration_since(*t).as_secs() < 5);
                }
                s.typing.retain(|_, set| !set.is_empty());
            }
        }
    });

    // macOS traffic lights overlay content; padding must be on the top row
    // only to avoid shifting the grid.
    let mac_top_pad = if cfg!(target_os = "macos") {
        "pt-5"
    } else {
        ""
    };
    let mac_titlebar_clear = if cfg!(target_os = "macos") {
        "pt-2 pl-20"
    } else {
        ""
    };

    rsx! {
        div { class: "h-full w-full flex flex-col bg-[var(--bg)] p-2 gap-2 {mac_top_pad}",
            style: "{guild_accent_style}",
            VoiceSounds {}
            VoiceSpeakingBridge {}
            ErrorToast {}
            crate::features::activities::ActivityHost {}
            crate::features::screenshare::ScreenShareBridge {}
            crate::features::screenshare::ScreenSourcePicker {}
            crate::features::screenshare::ScreenSelfPreview {}
            crate::features::camera::CameraBridge {}
            crate::mediakey::MediaKeyBridge {}
            crate::features::camera::CameraSelfPreview {}
            crate::features::camera::CameraGridWindow {}
            crate::features::screenshare::ScreenWatchWindow {}
            crate::features::profiles::ProfileCard {}
            crate::features::chat::ImageViewer {}
            GuildDialogHost {}

            // Row is a drag region; interactive children opt out via .dxf-no-
            // drag.
            div { class: "dxf-drag-region flex items-center gap-2 {mac_titlebar_clear}",
                onmousedown: move |_| crate::app::start_window_drag(),
                div { class: "shrink-0 flex items-center gap-2 px-1",
                    crate::app::DiscordiaLogo { class: "w-6 h-6" }
                    span { class: "dxf-display dxf-wordmark text-lg font-bold tracking-tight", "Discordia" }
                }
                // Says the state rather than leaving its absence to be
                // inferred from empty panels — which is what "nothing works"
                // looks like from the outside.
                if status == ConnectionStatus::Offline {
                    span { class: "shrink-0 px-2 py-0.5 rounded-full border border-[var(--border)] text-[10px] font-mono text-[var(--text-dim)]",
                        title: "Direct messages run on Nostr relays and need no server. Communities, channels and voice do.",
                        "no server"
                    }
                }
                HostBanner {}
                TransportBadge {}
                EncryptionBadge {}
                div { class: "dxf-no-drag shrink-0 flex items-center gap-2",
                    onmousedown: move |e| e.stop_propagation(),
                    // Only where there is something to unplug from. Offline is
                    // where this button lands you, so offering it there would be
                    // a control that does nothing.
                    if status != ConnectionStatus::Offline {
                        button {
                            class: "w-8 h-8 flex items-center justify-center rounded-md border border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--danger)] hover:border-[var(--danger)] transition-colors",
                            title: "Disconnect",
                            onclick: move |_| on_disconnect.call(String::new()),
                            dangerous_inner_html: UNPLUG_ICON_SVG,
                        }
                    }
                }
            }

            if let Some(text) = notice.clone() {
                div { class: "shrink-0 flex items-start gap-2 px-3 py-2 rounded-lg border border-[var(--danger)] bg-[var(--panel)]",
                    span { class: "text-[var(--danger)] text-sm leading-none mt-0.5", "!" }
                    span { class: "flex-1 min-w-0 text-xs text-[var(--text)]",
                        "The connection ended: {text}"
                    }
                }
            }

            // overflow-auto still matters in Snap mode: a panel dragged below
            // the last template row lands on `grid-auto-rows` and would
            // otherwise be clipped with no way to scroll to it. Free mode's own
            // container is exactly 100% tall and clips deliberately, so this
            // never produces a scrollbar there.
            div { class: "flex-1 overflow-auto min-h-0",
                // Use CSS grid `1fr` rows instead of pixel measurement so the
                // browser re-divides height on resize; the old `onmounted`
                // measurement caused panels to keep their original height when
                // the window grew.
                GridLayout {
                    cols: 12, rows: GRID_ROWS, gap: GRID_GAP,
                    store: layout, editable: edit_mode(),
                    // Free placement only; the Snap/Free switch was removed
                    // because maintaining two coordinate systems and
                    // conversions caused broken layouts, while free placement
                    // with magnetic edges achieves the same tidy arrangement
                    // without fighting a grid.
                    mode: LayoutMode::Free,
                    on_change: move |_: Vec<(String, GridPosition)>| persist_layout(settings, layout),
                    GridItem { id: "guilds", x: 0, y: 0, w: 1, h: GRID_ROWS, min_w: 1, min_h: 10,
                        GuildsSidebar {}
                    }
                    GridItem { id: "channels", x: 1, y: 0, w: 2, h: GRID_ROWS, min_w: 2, min_h: 10,
                        ChannelsColumn {}
                    }
                    GridItem { id: "chat", x: 3, y: 0, w: 7, h: GRID_ROWS, min_w: 3, min_h: 10,
                        div { class: "panel-hover w-full h-full flex flex-col bg-[var(--panel)] border border-[var(--border)] rounded-lg overflow-hidden",
                            if status == ConnectionStatus::Connecting {
                                div { class: "flex-1 flex items-center justify-center text-[var(--text-muted)] text-sm",
                                    "Connecting…"
                                }
                            } else if matches!(home_pane, Some(HomeView::Communities) | Some(HomeView::Servers)) {
                                crate::features::home::HomePane { on_switch }
                            } else if home_empty {
                                crate::features::home::HomeWelcome {}
                            } else {
                                ChatView {}
                            }
                            // Under the conversation only. The explore panes
                            // reach both levels through home's own column, so
                            // carrying the strip there would be a second copy
                            // of the navigation you are already standing in.
                            if home_pane == Some(HomeView::Dms) && status != ConnectionStatus::Connecting {
                                crate::features::home::ExploreStrip {}
                            }
                        }
                    }
                    GridItem { id: "members", x: 10, y: 0, w: 2, h: GRID_ROWS, min_w: 2, min_h: 10,
                        // A guild roster is a guild's answer, and home is not
                        // in one — the panel was simply empty there.
                        if home_pane.is_some() {
                            crate::features::home::PeoplePanel {}
                        } else {
                            MembersPanel {}
                        }
                    }
                }
            }

            div { class: "fixed bottom-3 right-3 z-40 flex items-center gap-1.5",
                // Reset is only offered while editing to allow recovery from
                // messy layouts or awkwardly dragged windows in Free mode.
                if edit_mode() {
                    button {
                        class: "border border-[var(--border)] rounded px-3 py-1 text-[10px] uppercase tracking-wider bg-[var(--panel)] hover:border-[var(--danger)] text-[var(--text-muted)] hover:text-[var(--danger)] transition-colors",
                        title: "Put the panels back the way they started",
                        onclick: move |_| {
                            layout.restore(default_layout(), Vec::new());
                            persist_layout(settings, layout);
                        },
                        "Reset"
                    }
                }
                button {
                    class: "border border-[var(--border)] rounded px-3 py-1 text-[10px] uppercase tracking-wider bg-[var(--panel)] hover:border-[var(--accent)] text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
                    onclick: move |_| edit_mode.set(!edit_mode()),
                    if edit_mode() { "Done" } else { "Edit layout" }
                }
            }
        }
    }
}

/// Bottom-center toast for `ServerMessage::Error` frames — permission and
/// moderation rejections must be visible, not just logged. Auto-dismisses
/// after a few seconds; click ✕ to dismiss sooner.
#[component]
fn ErrorToast() -> Element {
    let mut state = use_app_state();
    let message = use_memo(move || state.read().error_toast.clone());

    use_effect(move || {
        let Some(current) = message() else { return };
        spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(6)).await;
            let mut s = state.write();
            if s.error_toast.as_deref() == Some(current.as_str()) {
                s.error_toast = None;
            }
        });
    });

    let Some(msg) = message() else {
        return rsx! { Fragment {} };
    };

    rsx! {
        div { class: "dxf-pop-in fixed bottom-12 left-1/2 -translate-x-1/2 z-50 max-w-md flex items-center gap-2 px-3 py-2 rounded-lg border border-[var(--danger)]/50 bg-[var(--panel-solid)] shadow-xl",
            span { class: "w-2 h-2 rounded-full shrink-0", style: "background: var(--danger);" }
            span { class: "text-xs text-[var(--text)] flex-1", "{msg}" }
            button {
                class: "text-[var(--text-dim)] hover:text-[var(--text)] text-sm leading-none",
                onclick: move |_| state.write().error_toast = None,
                "✕"
            }
        }
    }
}

/// UI sound effects, synthesised with Web Audio so they need no assets.
///
/// Everything routes through one idempotent `window.dxSfx` object with a
/// per-cue cooldown. Two things make the naive approach double up: an effect
/// that re-runs because some *other* signal it reads changed, and a component
/// that gets mounted twice (hot reload, a re-keyed parent). The Rust side
/// guards the first with explicit last-value comparisons; the cooldown catches
/// anything that slips past, so a cue can't fire twice for one event.
const SFX_JS: &str = r#"
window.dxSfx = window.dxSfx || (function () {
  let ctx = null;
  const lastAt = {};
  // Cooldown is per cue name rather than global because cues can legitimately
  // overlap (e.g., leaving voice closes a share you were watching).
  const COOLDOWN_MS = 250;
  let masterVolume = 0.7;
  function audio() {
    if (!ctx) { try { ctx = new (window.AudioContext || window.webkitAudioContext)(); } catch (e) { return null; } }
    // Resume is a no-op if already running; autoplay policies suspend the
    // context until a gesture.
    if (ctx.state === 'suspended') { ctx.resume().catch(function () {}); }
    return ctx;
  }
  // Exponential ramps avoid zero (undefined for exponentialRampToValueAtTime)
  // to prevent clicks.
  // `peak` is scaled by masterVolume so user volume applies uniformly.
  function tone(c, at, freq, dur, peak, type) {
    const o = c.createOscillator(); const g = c.createGain();
    o.type = type || 'sine'; o.frequency.setValueAtTime(freq, at);
    o.connect(g); g.connect(c.destination);
    const p = (peak * masterVolume) || 0.0001;
    g.gain.setValueAtTime(0.0001, at);
    g.gain.exponentialRampToValueAtTime(p, at + 0.012);
    g.gain.exponentialRampToValueAtTime(0.0001, at + dur);
    o.start(at); o.stop(at + dur + 0.02);
  }
  function sweep(c, at, f0, f1, dur, peak, type) {
    const o = c.createOscillator(); const g = c.createGain();
    o.type = type || 'sawtooth';
    o.frequency.setValueAtTime(f0, at);
    o.frequency.linearRampToValueAtTime(f1, at + dur);
    o.connect(g); g.connect(c.destination);
    const p = (peak * masterVolume) || 0.0001;
    g.gain.setValueAtTime(0.0001, at);
    g.gain.exponentialRampToValueAtTime(p, at + 0.015);
    g.gain.exponentialRampToValueAtTime(0.0001, at + dur);
    o.start(at); o.stop(at + dur + 0.02);
  }
  function play(name) {
    const now = Date.now();
    if (lastAt[name] && now - lastAt[name] < COOLDOWN_MS) return;
    lastAt[name] = now;
    const c = audio(); if (!c) return;
    const t = c.currentTime;
    switch (name) {
      case 'disconnect':
        tone(c, t, 520, 0.14, 0.13); tone(c, t + 0.10, 330, 0.22, 0.13);
        break;
      // Opening someone's screen share: a quick rising blip, quieter than the
      // voice cues because it accompanies a visible window appearing.
      case 'watch-start':
        tone(c, t, 620, 0.09, 0.09, 'triangle'); tone(c, t + 0.07, 880, 0.14, 0.09, 'triangle');
        break;
      case 'watch-stop':
        tone(c, t, 720, 0.09, 0.07, 'triangle'); tone(c, t + 0.07, 480, 0.13, 0.07, 'triangle');
        break;
      case 'notify':
        tone(c, t, 660, 0.3, 0.14);
        break;
      case 'connect':
        tone(c, t, 440, 0.12, 0.12); tone(c, t + 0.09, 660, 0.18, 0.12);
        break;
      case 'server-disconnect':
        tone(c, t, 440, 0.15, 0.11); tone(c, t + 0.12, 220, 0.25, 0.11);
        break;
      case 'peer-join':
        tone(c, t, 523, 0.06, 0.08, 'triangle'); tone(c, t + 0.05, 659, 0.10, 0.08, 'triangle');
        break;
      case 'peer-leave':
        tone(c, t, 659, 0.06, 0.08, 'triangle'); tone(c, t + 0.05, 523, 0.10, 0.08, 'triangle');
        break;
      case 'stream-start':
        sweep(c, t, 300, 900, 0.18, 0.10, 'sawtooth');
        break;
      case 'stream-stop':
        sweep(c, t, 900, 300, 0.18, 0.10, 'sawtooth');
        break;
      case 'peer-stream-start':
        tone(c, t, 440, 0.07, 0.06, 'triangle'); tone(c, t + 0.06, 880, 0.12, 0.06, 'triangle');
        break;
      case 'peer-stream-stop':
        tone(c, t, 880, 0.07, 0.06, 'triangle'); tone(c, t + 0.06, 440, 0.12, 0.06, 'triangle');
        break;
      case 'mute':
        tone(c, t, 220, 0.06, 0.10, 'square');
        break;
      case 'unmute':
        tone(c, t, 440, 0.06, 0.10, 'square');
        break;
    }
  }
  function setVolume(v) { masterVolume = Math.max(0, Math.min(1, v)); }
  return { play: play, setVolume: setVolume };
})();
"#;

fn sfx(name: &str) {
    // Route through `js_str` to enforce escaping at the sink, preventing
    // future dynamic args from bypassing it.
    let name = crate::features::screenshare::js_str(name);
    let _ = document::eval(&format!("{SFX_JS}\nwindow.dxSfx.play({name});"));
}

#[component]
fn VoiceSounds() -> Element {
    let state = use_app_state();
    let phase = use_memo(move || state.read().voice.phase);
    let mut last_phase = use_signal(|| VoicePhase::Idle);

    use_effect(move || {
        let now = phase();
        let prev = *last_phase.peek();
        if now == prev {
            return;
        }
        last_phase.set(now);
        match now {
            VoicePhase::Connected => sfx("connect"),
            // Leaving voice, whether the user hung up or the session died.
            // Connecting → Idle is a cancelled/failed attempt that never made
            // a connect sound, so it gets no disconnect sound either.
            VoicePhase::Idle | VoicePhase::Error if prev == VoicePhase::Connected => {
                sfx("disconnect")
            }
            _ => {}
        }
    });

    let viewing = use_memo(move || state.read().screen_viewing.clone());
    let mut last_viewing = use_signal(|| None::<String>);
    use_effect(move || {
        let now = viewing();
        if now == *last_viewing.peek() {
            return;
        }
        let was_watching = last_viewing.peek().is_some();
        last_viewing.set(now.clone());
        match now {
            // Switching straight from one sharer to another still counts as
            // starting to watch — the stream on screen changed.
            Some(_) => sfx("watch-start"),
            None if was_watching => sfx("watch-stop"),
            None => {}
        }
    });

    let notify = use_memo(move || state.read().notify_tick);
    let mut last_notify = use_signal(|| 0u64);
    use_effect(move || {
        let now = notify();
        if now != 0 && now != *last_notify.peek() {
            sfx("notify");
        }
        last_notify.set(now);
    });

    let status = use_memo(move || state.read().status);
    let mut last_status = use_signal(|| ConnectionStatus::Connecting);
    use_effect(move || {
        let now = status();
        let prev = *last_status.peek();
        if now == prev {
            return;
        }
        last_status.set(now);
        match (prev, now) {
            (ConnectionStatus::Connecting, ConnectionStatus::Ready) => sfx("connect"),
            (_, ConnectionStatus::Disconnected) => sfx("server-disconnect"),
            _ => {}
        }
    });

    let muted = use_memo(move || state.read().voice.muted);
    let mut last_muted = use_signal(|| false);
    use_effect(move || {
        let now = muted();
        if now == *last_muted.peek() {
            return;
        }
        last_muted.set(now);
        if now {
            sfx("mute");
        } else {
            sfx("unmute");
        }
    });

    let sharing = use_memo(move || state.read().screen_sharing);
    let mut last_sharing = use_signal(|| false);
    use_effect(move || {
        let now = sharing();
        if now == *last_sharing.peek() {
            return;
        }
        last_sharing.set(now);
        if now {
            sfx("stream-start");
        } else {
            sfx("stream-stop");
        }
    });

    // On channel switch, snapshot without playing sounds — the set changed
    // because we moved, not because peers did.
    let voice_channel = use_memo(move || state.read().voice.channel_id);
    let voice_states = use_memo(move || state.read().voice_states.clone());
    let self_pk = use_memo(move || state.read().self_user.as_ref().map(|u| u.pubkey.clone()));
    let mut last_channel = use_signal(|| None::<Id>);
    let mut last_peers = use_signal(Vec::<String>::new);
    use_effect(move || {
        let ch = voice_channel();
        let states = voice_states();
        let me = self_pk();
        let peers: Vec<String> = match (&ch, &me) {
            (Some(cid), Some(me_pk)) => states
                .iter()
                .filter(|v| v.channel_id == Some(*cid) && &v.user_pubkey != me_pk)
                .map(|v| v.user_pubkey.clone())
                .collect(),
            _ => Vec::new(),
        };
        if ch != *last_channel.peek() {
            last_channel.set(ch);
            last_peers.set(peers);
            return;
        }
        let prev = last_peers.peek().clone();
        let joined = peers.iter().any(|p| !prev.contains(p));
        let left = prev.iter().any(|p| !peers.contains(p));
        if joined {
            sfx("peer-join");
        }
        if left {
            sfx("peer-leave");
        }
        last_peers.set(peers);
    });

    let screen_shares = use_memo(move || state.read().screen_shares.clone());
    let mut last_sharers = use_signal(Vec::<String>::new);
    use_effect(move || {
        let ch = voice_channel();
        let shares = screen_shares();
        let me = self_pk();
        let sharers: Vec<String> = match (&ch, &me) {
            (Some(cid), Some(me_pk)) => shares
                .get(cid)
                .map(|v| v.iter().filter(|p| *p != me_pk).cloned().collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        // Effects may fire in different orders, so we need our own snapshot
        // here.
        if ch != *last_channel.peek() {
            last_sharers.set(sharers);
            return;
        }
        let prev = last_sharers.peek().clone();
        let started = sharers.iter().any(|p| !prev.contains(p));
        let stopped = prev.iter().any(|p| !sharers.contains(p));
        if started {
            sfx("peer-stream-start");
        }
        if stopped {
            sfx("peer-stream-stop");
        }
        last_sharers.set(sharers);
    });

    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let sfx_vol = use_memo(move || settings.read().sfx_volume);
    use_effect(move || {
        let vol = sfx_vol();
        let v = vol as f32 / 100.0;
        let _ = document::eval(&format!("{SFX_JS}\nwindow.dxSfx.setVolume({v});"));
    });

    rsx! { Fragment {} }
}

#[component]
fn VoiceSpeakingBridge() -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let speaking = use_memo(move || state.read().voice.speaking);
    let mut last = use_signal(|| false);

    use_effect(move || {
        let now = speaking();
        if now != *last.peek() {
            last.set(now);
            gateway.send(ClientMessage::SetSpeaking { speaking: now });
        }
    });

    rsx! { Fragment {} }
}

#[component]
fn HostBanner() -> Element {
    let state = use_app_state();
    let snapshot = state.read();
    let Some(info) = snapshot.host_info.clone() else {
        return rsx! { Fragment {} };
    };
    drop(snapshot);

    let lan_text = info.lan_url.clone();
    let shortcode = info.shortcode.clone();
    let publish_error = info.publish_error.clone();
    let listed_public = info.listed_public;
    let has_shortcode = info.shortcode.is_some();
    let reachability = info.reachability.clone();
    // Whether voice works is "is there an SFU", not "did we start one" — a
    // rendezvous that runs its own means we deliberately started nothing.
    let (voice_label, voice_color) = if info.livekit_url.is_empty() {
        ("voice unavailable", "text-[var(--warn)]")
    } else if info.voice_bundled {
        ("voice ready", "text-[var(--success)]")
    } else {
        ("voice via rendezvous", "text-[var(--success)]")
    };

    rsx! {
        div { class: "panel-hover flex-1 min-w-0 px-3 py-2 bg-[var(--panel)] border border-[var(--border)] rounded-lg flex items-center gap-3 text-xs flex-wrap",
            span { class: "text-[var(--accent)] font-medium tracking-wide", "Self-hosting" }
            if let Some(code) = shortcode {
                span { class: "text-[var(--text-dim)]", "·" }
                span { class: "text-[var(--text-muted)]", "Code:" }
                code { class: "text-[var(--text)] select-all font-medium",
                    onmousedown: move |e| e.stop_propagation(),
                    "{code}"
                }
            }
            span { class: "text-[var(--text-dim)]", "·" }
            span { class: "text-[var(--text-muted)]", "LAN:" }
            code { class: "text-[var(--text)] select-all",
                onmousedown: move |e| e.stop_propagation(),
                "{lan_text}"
            }
            // Publishing status: a failed registration used to be silent, so
            // the host thought friends could find them when they couldn't.
            if let Some(err) = publish_error {
                span { class: "text-[var(--text-dim)]", "·" }
                span { class: "text-[var(--danger)]",
                    title: "{err}",
                    "⚠ not published — {err}"
                }
            } else if has_shortcode && !listed_public {
                span { class: "text-[var(--text-dim)]", "·" }
                span { class: "text-[var(--text-muted)]",
                    // Names no control, on purpose: the previous wording quoted
                    // a checkbox label and pointed at a "Browse" tab, and both
                    // were renamed out from under it.
                    title: "Reachable by code, but not shown in the public list. Turn on public listing when you create the server to appear there.",
                    "unlisted"
                }
            }
            span { class: "flex-1" }
            Reachability { reachability }
            span { class: "{voice_color}", "● {voice_label}" }
        }
    }
}

/// How far this host reaches, in the banner, with the reason when it is short.
///
/// A host that cannot be reached from the internet is the normal outcome behind
/// carrier-grade NAT and on a router with UPnP off, and it used to be invisible:
/// the only symptom was a friend failing to connect, which looks like the
/// friend's problem. See `docs/NETWORKING.md`.
#[component]
fn Reachability(reachability: crate::host::Reachability) -> Element {
    use crate::host::Reachability as R;
    match reachability {
        R::Direct {
            endpoint,
            method,
            media,
        } => {
            let title = if media {
                format!(
                    "{method} mapped this machine's ports. Friends reach you at {endpoint} without the relay, voice included."
                )
            } else {
                format!(
                    "{method} mapped the chat port ({endpoint}), but not the voice ports — calls still go through a relay's SFU, or stay on this network."
                )
            };
            rsx! {
                span { class: "text-[var(--text-dim)]", "·" }
                span { class: "text-[var(--success)]", title: "{title}",
                    if media { "● reachable directly" } else { "● reachable directly (chat only)" }
                }
            }
        }
        R::LanOnly { reason } => rsx! {
            span { class: "text-[var(--text-dim)]", "·" }
            span { class: "text-[var(--warn)]",
                title: "{reason} Friends elsewhere can still join by code — the rendezvous relays them.",
                "● this network only"
            }
        },
        R::LoopbackOnly => rsx! {
            span { class: "text-[var(--text-dim)]", "·" }
            span { class: "text-[var(--text-muted)]",
                title: "Direct connections weren't enabled, so the gateway only listens on this machine. Friends can still join by code through the rendezvous.",
                "● local only"
            }
        },
    }
}

/// Whether this call's media is encrypted, and whether that is currently
/// working.
///
/// The second half is why this exists at all. End-to-end encrypted media fails
/// as *silence*: frames arrive, decode to noise, and everything looks connected.
/// Somebody in that state will check their microphone, their output device and
/// their network before suspecting a key — so the one thing worth putting on
/// screen is that the key is the problem.
#[component]
fn EncryptionBadge() -> Element {
    let state = use_app_state();
    let snapshot = state.read();
    let in_voice = snapshot.voice.channel_id.is_some();
    let has_key = snapshot
        .voice
        .channel_id
        .is_some_and(|c| snapshot.media_keys.contains_key(&c));
    let broken = snapshot.media_undecryptable;
    drop(snapshot);

    if !in_voice || (!has_key && !broken) {
        return rsx! { Fragment {} };
    }

    let (label, color, title) = if broken {
        (
            "media undecryptable",
            "text-[var(--danger)]",
            "Encrypted media is arriving that this client cannot decrypt — usually a key that has not reached you yet. It often clears on its own within a second; if it does not, rejoin the channel.",
        )
    } else {
        (
            "media encrypted",
            "text-[var(--success)]",
            "Voice, screen share and camera are encrypted end to end. The SFU forwards frames it cannot decrypt — including one you run yourself.",
        )
    };
    rsx! {
        span { class: "shrink-0 px-2 py-1 text-[10px] uppercase tracking-wider {color}",
            title: "{title}",
            "{label}"
        }
    }
}

/// Who is carrying this connection, for anyone who is *not* hosting.
///
/// The distinction the badge exists for: a relayed connection is readable by
/// whoever runs the relay, and a direct one is not — but neither announces
/// itself, and a join by code can end up as either depending on a race.
#[component]
fn TransportBadge() -> Element {
    let state = use_app_state();
    let snapshot = state.read();
    // Self-host has its own banner, which says more than this would.
    if snapshot.host_info.is_some() {
        return rsx! { Fragment {} };
    }
    let transport = snapshot.transport;
    drop(snapshot);

    let (label, color, title) = match transport {
        crate::state::Transport::Loopback => return rsx! { Fragment {} },
        crate::state::Transport::Private => (
            "private",
            "text-[var(--success)]",
            "Connected straight to the host over an encrypted QUIC transport, with the host authenticated by its public key. Nobody in between can read this connection. The host can see your IP address.",
        ),
        crate::state::Transport::Direct => (
            "direct",
            "text-[var(--warn)]",
            "Connected straight to the host — no relay in the middle — but in the clear: every hop on the path can read this connection. The host can see your IP address.",
        ),
        crate::state::Transport::Relayed => (
            "relayed",
            "text-[var(--text-muted)]",
            "Carried by a rendezvous relay, which can read everything on this connection. The host never learns your address.",
        ),
    };
    rsx! {
        span { class: "shrink-0 px-2 py-1 text-[10px] uppercase tracking-wider {color}",
            title: "{title}",
            "{label}"
        }
    }
}

/// Renders whichever guild-management dialog is open, at the workspace root.
///
/// Deliberately not inside `GuildsSidebar`, where these used to live. Panels
/// are absolutely positioned and can be stacked above one another, and a panel
/// with a z-index establishes a stacking context — so a modal rendered inside
/// one is confined to that panel's layer and paints underneath any panel above
/// it, however high its own z-index is. At the root there is nothing to escape.
#[component]
fn GuildDialogHost() -> Element {
    let mut state = use_app_state();
    let open = use_memo(move || state.read().guild_dialog);
    let close = move |_| state.write().guild_dialog = None;

    match open() {
        None => rsx! { Fragment {} },
        Some(crate::state::GuildDialog::Settings(gid)) => rsx! {
            crate::features::guild_settings::GuildSettingsDialog { guild_id: gid, on_close: close }
        },
        Some(crate::state::GuildDialog::Integrations(gid)) => rsx! {
            crate::features::integrations::IntegrationsDialog { guild_id: gid, on_close: close }
        },
        Some(crate::state::GuildDialog::Roles(gid)) => rsx! {
            crate::features::roles::RolesDialog { guild_id: gid, on_close: close }
        },
    }
}
