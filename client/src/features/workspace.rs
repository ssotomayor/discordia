use dioxus::prelude::*;
use dioxus_grid_layout::{
    FloatRect, GridItem, GridLayout, GridPosition, LayoutMode, use_layout_store,
};

use crate::features::{
    channels::ChannelsColumn, chat::ChatView, guilds::GuildsSidebar, members::MembersPanel,
    voice::spawn_voice_service,
};
use crate::net::spawn_gateway;
use crate::protocol::{ClientMessage, Id};
use crate::state::{
    AppState, ConnectionStatus, SessionParams, VoicePhase, use_app_state, use_gateway,
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

#[component]
pub fn WorkspaceView(params: SessionParams, on_disconnect: EventHandler<String>) -> Element {
    let state = use_signal(AppState::empty);
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();

    let (gateway_tx, voice_tx) = use_hook(|| {
        // Restore the persisted audio preferences BEFORE the voice service
        // starts: it seeds its live audio controls from `AppState` on its first
        // poll. Without this the saved mic sensitivity, device choices and
        // noise-cancellation toggle were written to settings.json and then
        // silently ignored on every launch, which is most of why the
        // sensitivity slider looked like it did nothing.
        {
            let saved = settings.read();
            let mut app = state;
            let mut w = app.write();
            w.mic_sensitivity = saved.mic_sensitivity.clamp(1, 1000);
            w.mic_volume = saved.mic_volume.min(200);
            w.auto_gain_control = saved.auto_gain_control;
            w.noise_cancellation = saved.noise_cancellation;
            // Honoured only where there is processing to bypass; a settings
            // file carried over from a Windows machine must not leave a macOS
            // session believing it captures raw.
            w.bypass_system_audio_processing =
                saved.bypass_system_audio_processing && crate::rawmic::supported();
            // The slider's own domain, not DeepFilterNet's. `mic_sensitivity`
            // above clamps wider than its control because the two are in
            // different units; this one is bound straight to the dB value, so
            // without the clamp below a hand-edited 90 would print "90 dB max"
            // beside a slider that cannot reach it — and be handed to the
            // model regardless. It is the third use of the shared bounds, and
            // the one worth naming: the other two guard a control the user is
            // touching, this one guards a file they edited.
            w.denoise_atten_lim_db = saved.denoise_atten_lim_db.clamp(
                crate::features::voice::DENOISE_ATTEN_LIM_DB_MIN,
                crate::features::voice::DENOISE_ATTEN_LIM_DB_MAX,
            );
            // Anything outside the two offered values means a hand-edited
            // settings.json; fall back rather than publish an odd bitrate.
            w.voice_bitrate_kbps = match saved.voice_bitrate_kbps {
                24 => 24,
                _ => 48,
            };
            w.selected_input_device = saved.selected_input_device.clone();
            w.selected_output_device = saved.selected_output_device.clone();
        }
        let voice_tx = spawn_voice_service(state);
        let gateway_tx = spawn_gateway(params.clone(), state, voice_tx.clone(), move |reason| {
            on_disconnect.call(reason);
        });
        (gateway_tx, voice_tx)
    });
    provide_context(gateway_tx.clone());
    provide_context(crate::features::voice::VoiceTx(voice_tx.clone()));
    provide_context(state);
    // The Nostr identity (with signing key) — used to authorize Blossom uploads.
    provide_context(params.identity.clone());

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
    // Free-mode rects live in a second map, so they're restored separately.
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

    // Owner-set accent for the guild we're currently viewing (not in DM mode),
    // layered over the user's theme/accent while it's selected. Applied inline
    // on this subtree so it overrides the app-level accent.
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

    // Sweep stale typing indicators (older than 5s) so they fade out. Only
    // writes when there's something to prune, to avoid idle re-renders.
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

    // On macOS the traffic lights float over our content (fullsize content
    // view). Only the TOP ROW needs to dodge them: `pt-7` drops it below the
    // lights vertically and `pl-20` clears them horizontally. The padding must
    // NOT live on the outer column, or it would shove the whole widget grid
    // inward and leave a fat margin down the left edge.
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

            // Top row: host banner (only renders when self-hosting) grows
            // to push the brand mark + wallet button to the right. The
            // whole row is a drag region so the empty space between
            // elements lets the user move the window; the interactive
            // children opt out with .dxf-no-drag.
            div { class: "dxf-drag-region flex items-center gap-2 {mac_titlebar_clear}",
                onmousedown: move |_| crate::app::start_window_drag(),
                div { class: "shrink-0 flex items-center gap-2 px-1",
                    crate::app::DiscordiaLogo { class: "w-6 h-6" }
                    span { class: "dxf-display dxf-wordmark text-lg font-bold tracking-tight", "Discordia" }
                }
                HostBanner {}
                TransportBadge {}
                EncryptionBadge {}
                // Unplug / disconnect. Always present so the user can leave a
                // server they've connected to. Empty reason → clean return to
                // the connect screen (no error banner; see App::on_disconnect).
                div { class: "dxf-no-drag shrink-0 flex items-center",
                    onmousedown: move |e| e.stop_propagation(),
                    button {
                        class: "w-8 h-8 flex items-center justify-center rounded-md border border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--danger)] hover:border-[var(--danger)] transition-colors",
                        title: "Disconnect",
                        onclick: move |_| on_disconnect.call(String::new()),
                        dangerous_inner_html: UNPLUG_ICON_SVG,
                    }
                }
            }

            // overflow-auto still matters in Snap mode: a panel dragged below
            // the last template row lands on `grid-auto-rows` and would
            // otherwise be clipped with no way to scroll to it. Free mode's own
            // container is exactly 100% tall and clips deliberately, so this
            // never produces a scrollbar there.
            div { class: "flex-1 overflow-auto min-h-0",
                // No pixel measurement here any more. `rows` makes the grid use
                // `repeat(GRID_ROWS, 1fr)`, so the browser re-divides the height
                // on every window resize. The old code measured once in
                // `onmounted` and never again, which is why panels kept their
                // original height when the window grew.
                GridLayout {
                    cols: 12, rows: GRID_ROWS, gap: GRID_GAP,
                    store: layout, editable: edit_mode(),
                    // Free placement only. The Snap/Free switch is gone: two
                    // coordinate systems and a conversion between them was a
                    // steady source of broken layouts, and free placement with
                    // magnetic edges gets you a tidy arrangement anyway — you
                    // just don't have to fight a grid for it.
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
                            } else {
                                ChatView {}
                            }
                        }
                    }
                    GridItem { id: "members", x: 10, y: 0, w: 2, h: GRID_ROWS, min_w: 2, min_h: 10,
                        MembersPanel {}
                    }
                }
            }

            // Floating layout controls. Always visible, subtle.
            div { class: "fixed bottom-3 right-3 z-40 flex items-center gap-1.5",
                // Reset is only offered while editing — it is the way back from
                // a layout you've made a mess of, including a window dragged
                // somewhere awkward in Free mode.
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

    // Auto-clear ~6s after the latest error appears (unless it changed again).
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
  // Two cues can legitimately overlap (leaving voice closes a share you were
  // watching), so the cooldown is per cue name rather than global.
  const COOLDOWN_MS = 250;
  // Master volume multiplier (0..1), set from Rust via setVolume. Scales the
  // `peak` of every tone so a single knob controls all UI sound effects.
  let masterVolume = 0.7;
  function audio() {
    if (!ctx) { try { ctx = new (window.AudioContext || window.webkitAudioContext)(); } catch (e) { return null; } }
    // Autoplay policies suspend the context until a gesture; resume is a no-op
    // when it is already running.
    if (ctx.state === 'suspended') { ctx.resume().catch(function () {}); }
    return ctx;
  }
  // One short enveloped tone. Exponential ramps (never to exactly zero, which
  // is undefined for exponentialRampToValueAtTime) so there is no click.
  // `peak` is scaled by masterVolume so the user's volume setting applies
  // uniformly to every cue.
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
  // Frequency sweep for whoosh-style cues (stream start/stop). Linear ramp
  // from `f0` to `f1` over `dur`, same envelope as `tone`.
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
      // Leaving voice: a falling two-tone, the inverse of the connect chime.
      case 'disconnect':
        tone(c, t, 520, 0.14, 0.13); tone(c, t + 0.10, 330, 0.22, 0.13);
        break;
      // Opening someone's screen share: a quick rising blip, quieter than the
      // voice cues because it accompanies a visible window appearing.
      case 'watch-start':
        tone(c, t, 620, 0.09, 0.09, 'triangle'); tone(c, t + 0.07, 880, 0.14, 0.09, 'triangle');
        break;
      // Closing it again: same shape, descending, softer still.
      case 'watch-stop':
        tone(c, t, 720, 0.09, 0.07, 'triangle'); tone(c, t + 0.07, 480, 0.13, 0.07, 'triangle');
        break;
      case 'notify':
        tone(c, t, 660, 0.3, 0.14);
        break;
      // --- Connection ---
      // Connect: a rising two-tone, the counterpart to the disconnect fall.
      case 'connect':
        tone(c, t, 440, 0.12, 0.12); tone(c, t + 0.09, 660, 0.18, 0.12);
        break;
      // Server disconnect: lower and longer, signalling something went wrong.
      case 'server-disconnect':
        tone(c, t, 440, 0.15, 0.11); tone(c, t + 0.12, 220, 0.25, 0.11);
        break;
      // --- Voice room peers ---
      // Peer joined voice: a short ascending blip.
      case 'peer-join':
        tone(c, t, 523, 0.06, 0.08, 'triangle'); tone(c, t + 0.05, 659, 0.10, 0.08, 'triangle');
        break;
      // Peer left voice: same shape, descending.
      case 'peer-leave':
        tone(c, t, 659, 0.06, 0.08, 'triangle'); tone(c, t + 0.05, 523, 0.10, 0.08, 'triangle');
        break;
      // --- Screen share (self) ---
      // Stream start: a quick rising whoosh.
      case 'stream-start':
        sweep(c, t, 300, 900, 0.18, 0.10, 'sawtooth');
        break;
      // Stream stop: descending whoosh.
      case 'stream-stop':
        sweep(c, t, 900, 300, 0.18, 0.10, 'sawtooth');
        break;
      // --- Screen share (peer) ---
      // Peer started streaming: soft rising blip, quieter than self cues.
      case 'peer-stream-start':
        tone(c, t, 440, 0.07, 0.06, 'triangle'); tone(c, t + 0.06, 880, 0.12, 0.06, 'triangle');
        break;
      // Peer stopped streaming: soft descending blip.
      case 'peer-stream-stop':
        tone(c, t, 880, 0.07, 0.06, 'triangle'); tone(c, t + 0.06, 440, 0.12, 0.06, 'triangle');
        break;
      // --- Self mute/unmute ---
      // Mute: a low short click.
      case 'mute':
        tone(c, t, 220, 0.06, 0.10, 'square');
        break;
      // Unmute: a higher short click.
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
    let _ = document::eval(&format!("{SFX_JS}\nwindow.dxSfx.play('{name}');"));
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

    // Opening / closing someone else's screen share.
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

    // Notification chime for inbound DMs / mentions.
    let notify = use_memo(move || state.read().notify_tick);
    let mut last_notify = use_signal(|| 0u64);
    use_effect(move || {
        let now = notify();
        if now != 0 && now != *last_notify.peek() {
            sfx("notify");
        }
        last_notify.set(now);
    });

    // --- Server connection status: connect / server-disconnect ---
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

    // --- Self mute / unmute ---
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

    // --- Self screen share start / stop ---
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

    // --- Peer joined / left voice ---
    // Watch the set of pubkeys in our voice channel (excluding self). When it
    // grows someone joined; when it shrinks someone left. On channel switch we
    // snapshot without playing sounds — the whole set changed because *we*
    // moved, not because peers did.
    let voice_channel = use_memo(move || state.read().voice.channel_id);
    let voice_states = use_memo(move || state.read().voice_states.clone());
    let self_pk = use_memo(move || state.read().self_user.as_ref().map(|u| u.pubkey.clone()));
    let mut last_channel = use_signal(|| None::<Id>);
    let mut last_peers = use_signal(Vec::<String>::new);
    use_effect(move || {
        let ch = voice_channel();
        let states = voice_states();
        let me = self_pk();
        // Snapshot the peers in our channel, excluding self.
        let peers: Vec<String> = match (&ch, &me) {
            (Some(cid), Some(me_pk)) => states
                .iter()
                .filter(|v| v.channel_id == Some(*cid) && &v.user_pubkey != me_pk)
                .map(|v| v.user_pubkey.clone())
                .collect(),
            _ => Vec::new(),
        };
        // Channel changed (we joined/switched) — snapshot without sounds.
        if ch != *last_channel.peek() {
            last_channel.set(ch);
            last_peers.set(peers);
            return;
        }
        let prev = last_peers.peek().clone();
        // Diff: new pubkeys that weren't before = join, gone = leave.
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

    // --- Peer screen share start / stop ---
    // Same pattern: watch the sharers in our channel excluding self, diff
    // against the last snapshot, snapshot silently on channel switch.
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
        // Channel changed — the peer-join watcher already updated last_channel,
        // but we need our own snapshot here too since this effect may fire in a
        // different order.
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

    // --- Master volume: apply sfx_volume from settings on startup ---
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
                    title: "Reachable by code, but hidden from the Browse list. Enable \"List this server publicly\" when self-hosting to appear there.",
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
    // Only meaningful in a call.
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
