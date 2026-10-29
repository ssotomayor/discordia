use dioxus::prelude::*;

use crate::features::{account::AccountView, identity_setup::IdentitySetupView};
use crate::identity::Identity;
use crate::session::{self, SavedSession};
use crate::state::{SessionMode, SessionParams};
use url::Url;

#[cfg(target_os = "windows")]
static TRAY_QUIT_RECEIVER: std::sync::OnceLock<
    std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<()>>>,
> = std::sync::OnceLock::new();
#[cfg(target_os = "windows")]
static TRAY_RESTORE_RECEIVER: std::sync::OnceLock<
    std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<()>>>,
> = std::sync::OnceLock::new();
#[cfg(target_os = "windows")]
static TRAY_RESTORE_SENDER: std::sync::OnceLock<tokio::sync::mpsc::UnboundedSender<()>> =
    std::sync::OnceLock::new();
#[cfg(target_os = "windows")]
static TRAY_QUIT_AVAILABLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "windows")]
pub fn request_restore() {
    if let Some(sender) = TRAY_RESTORE_SENDER.get() {
        let _ = sender.send(());
    }
}

#[cfg(target_os = "windows")]
pub fn init_tray_quit_bridge() {
    TRAY_QUIT_AVAILABLE.store(true, std::sync::atomic::Ordering::SeqCst);

    let (quit_sender, quit_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (restore_sender, restore_receiver) = tokio::sync::mpsc::unbounded_channel();

    let _ = TRAY_RESTORE_SENDER.set(restore_sender.clone());

    if TRAY_QUIT_RECEIVER
        .set(std::sync::Mutex::new(Some(quit_receiver)))
        .is_err()
    {
        tracing::warn!("Tray quit bridge was already initialized");
        return;
    }
    if TRAY_RESTORE_RECEIVER
        .set(std::sync::Mutex::new(Some(restore_receiver)))
        .is_err()
    {
        tracing::warn!("Tray restore bridge was already initialized");
        return;
    }

    crate::single_instance::init_restore_bridge(restore_sender.clone());

    crate::platform::trayicon::menu::MenuEvent::set_event_handler(Some(
        move |event: crate::platform::trayicon::menu::MenuEvent| {
            if event.id.0.as_str() == "discordia-quit" {
                tracing::info!("Received exit request from tray menu");
                if quit_sender.send(()).is_err() {
                    tracing::warn!("Tray quit request could not reach the app");
                }
            }
        },
    ));
    crate::platform::trayicon::TrayIconEvent::set_event_handler(Some(
        move |event: crate::platform::trayicon::TrayIconEvent| {
            if matches!(
                event,
                crate::platform::trayicon::TrayIconEvent::Click {
                    button: crate::platform::trayicon::MouseButton::Left,
                    ..
                }
            ) {
                tracing::info!("Received restore request from tray icon");
                if restore_sender.send(()).is_err() {
                    tracing::warn!("Tray restore request could not reach the app");
                }
            }
        },
    ));
}

const DISCORDIA_LOGO_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" role="img" aria-label="Discordia">
  <defs>
    <linearGradient id="dxfYellowGrad" x1="250" y1="325" x2="565" y2="760" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#ffd21a"/>
      <stop offset="0.45" stop-color="#ffdb24"/>
      <stop offset="1" stop-color="#ffc400"/>
    </linearGradient>
    <radialGradient id="dxfDarkGrad" cx="610" cy="445" r="340" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#2d303c"/>
      <stop offset="0.65" stop-color="#242733"/>
      <stop offset="1" stop-color="#181b25"/>
    </radialGradient>
    <filter id="dxfSoftShadow" x="-15%" y="-15%" width="130%" height="130%">
      <feDropShadow dx="0" dy="7" stdDeviation="7" flood-color="#000000" flood-opacity="0.30"/>
    </filter>
  </defs>
  <g filter="url(#dxfSoftShadow)">
    <g class="dxf-logo-left">
      <path d="M315 301 C253 354 223 428 230 507 C237 598 288 684 370 727 C421 754 481 766 553 761 L455 659 L499 609 L376 530 L445 434 Z" fill="url(#dxfYellowGrad)"/>
    </g>
    <g class="dxf-logo-right">
      <path d="M326 278 C420 218 553 205 657 245 C752 282 814 366 828 467 C842 570 801 667 724 719 C686 745 639 759 584 758 L503 661 L558 602 L432 530 L490 434 Z" fill="url(#dxfDarkGrad)" stroke="#11141c" stroke-width="2"/>
      <path d="M323 278 L490 434 L432 530 L558 602 L503 661 L584 758" fill="none" stroke="#050608" stroke-width="18" stroke-linejoin="miter" stroke-linecap="butt" opacity="0.92"/>
      <path d="M326 278 C420 218 553 205 657 245 C752 282 814 366 828 467 C842 570 801 667 724 719 C686 745 639 759 584 758 L503 661 L558 602 L432 530 L490 434 Z" fill="url(#dxfDarkGrad)" stroke="#11141c" stroke-width="2"/>
      <rect x="588" y="395" width="70" height="164" rx="35" ry="35" fill="url(#dxfYellowGrad)" stroke="#050608" stroke-width="2"/>
    </g>
  </g>
  <g class="dxf-splat">
    <circle cx="418" cy="468" r="11" fill="#ffd21a"/>
    <circle cx="560" cy="456" r="8" fill="#2d303c"/>
    <circle cx="596" cy="520" r="12" fill="#ffd21a"/>
    <circle cx="540" cy="586" r="7" fill="#ffc400"/>
    <circle cx="448" cy="592" r="9" fill="#2d303c"/>
    <circle cx="398" cy="540" r="7" fill="#ffd21a"/>
    <circle cx="512" cy="414" r="9" fill="#ffdb24"/>
    <circle cx="470" cy="648" r="6" fill="#2d303c"/>
    <circle cx="608" cy="566" r="7" fill="#ffdb24"/>
    <circle cx="428" cy="416" r="6" fill="#ffc400"/>
    <circle cx="556" cy="646" r="8" fill="#ffd21a"/>
  </g>
</svg>"##;

pub fn start_window_drag() {
    crate::platform::window().drag();
}

pub fn open_external(url: &str) {
    let Some(url) = openable(url) else { return };
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::{HSTRING, PCWSTR, w};
        let target = HSTRING::from(url.as_str());
        // SAFETY: every pointer outlives the call and ShellExecuteW does not retain them.
        unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                &target,
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        #[cfg(target_os = "macos")]
        let opener = "open";
        #[cfg(not(target_os = "macos"))]
        let opener = "xdg-open";
        let _ = std::process::Command::new(opener).arg(url.as_str()).spawn();
    }
}

/// Chat text reaches this. Anything but a parsed web URL is refused because the
/// opener dispatches on scheme, and `cmd /C start` once read `&` as a command.
fn openable(url: &str) -> Option<Url> {
    let url = Url::parse(url).ok()?;
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some()).then_some(url)
}

#[component]
pub fn DiscordiaLogo(
    #[props(into)] class: String,
    #[props(default = false)] compact: bool,
) -> Element {
    static COMPACT: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        DISCORDIA_LOGO_SVG.replace("viewBox=\"0 0 1024 1024\"", "viewBox=\"200 190 650 630\"")
    });
    let svg = if compact {
        COMPACT.as_str()
    } else {
        DISCORDIA_LOGO_SVG
    };
    rsx! {
        div {
            class: "dxf-logo {class}",
            dangerous_inner_html: svg,
        }
    }
}

// Generated by the Tailwind watcher dx starts, and committed: a plain
// `cargo build` has no dx to regenerate it.
const TAILWIND_CSS: &str = include_str!("../assets/tailwind.css");

/// Inlined, not `src`: a machine with no route to the internet must still be
/// able to show a share.
const LIVEKIT_JS: &str = include_str!("../assets/livekit-client.umd.js");

const LIVEKIT_E2EE_WORKER_JS: &str = include_str!("../assets/livekit-client.e2ee.worker.js");

/// Runs after the worker's own script so it can wrap the handler that script
/// installed. Keeps every CryptoKey inside the worker; see the file.
const E2EE_WORKER_SHIM_JS: &str = include_str!("../assets/e2ee-worker-shim.js");

const BASE_CSS: &str = "
/* Variable *names* are the styling interface, so new palette entries are added
   rather than renamed; `theme_vars()` overrides these inline on the app root. */
:root {
  --bg: #121110; --bg2: #1e1b17;
  --panel-solid: #17150f; --panel: #17150f; --panel2: #1b1813;
  --edge: #262219; --edge-strong: #322c24;
  --border: #2a2621; --border-strong: #3a342b;
  --text: #f2ece3; --text-muted: #9a9287; --text-dim: #857d72;
  --accent: #e8933a; --accent-soft: rgba(232,147,58,.16); --accent-strong: #f5ab5c;
  --up: #6dbf6a; --success: #6dbf6a;
  --violet: #b98cff; --amber: #ffc46b; --warn: #ffc46b;
  --danger: #e8776a;
  --ease: cubic-bezier(0.4, 0.0, 0.2, 1);
  --safe-top: env(safe-area-inset-top, 0px);
  --safe-bottom: env(safe-area-inset-bottom, 0px);
  --safe-left: env(safe-area-inset-left, 0px);
  --safe-right: env(safe-area-inset-right, 0px);
}
html, body, #main { height: 100%; margin: 0; }
.dxf-home-tools { padding-right: 44px; }
.dxf-home-settings { position: absolute; top: 8px; right: 8px; }
.dxf-home-macos .dxf-home-settings { top: 16px; }
.dxf-workspace-mobile .dxf-workspace-header { display: grid; grid-template-columns: 24px minmax(0, 1fr) auto; gap: 4px; flex-shrink: 0; min-width: 0; }
.dxf-workspace-mobile .dxf-workspace-brand { grid-column: 1; grid-row: 1; }
.dxf-workspace-mobile .dxf-workspace-tools { grid-column: 3; grid-row: 1; margin-left: 0; gap: 0; }
.dxf-workspace-mobile .dxf-workspace-brand span,
.dxf-workspace-mobile .dxf-disconnect-label { display: none; }
.dxf-workspace-mobile .dxf-connection-controls { grid-column: 2; grid-row: 1; width: 100%; max-width: 100%; min-width: 0; flex-wrap: nowrap; }
.dxf-workspace-mobile .dxf-connection-controls > button:not(.dxf-server-disconnect):not(.dxf-encryption-badge) { flex: 1 1 0; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dxf-encryption-icon { display: none; }
.dxf-route-mobile { display: none; }
.dxf-workspace-mobile .dxf-route-full { display: none; }
.dxf-workspace-mobile .dxf-route-mobile { display: inline; }
.dxf-workspace-mobile .dxf-encryption-badge { flex: 0 0 44px; padding: 0; }
.dxf-workspace-mobile .dxf-encryption-label { display: none; }
.dxf-workspace-mobile .dxf-encryption-icon { display: inline-block; width: 16px; height: 16px; vertical-align: middle; }
.dxf-workspace-mobile .dxf-encryption-icon svg { width: 100%; height: 100%; }
.dxf-workspace-mobile .dxf-server-disconnect { justify-content: center; flex: 0 0 44px; padding: 0; }
.dxf-workspace-mobile .dxf-workspace-header button { min-height: 44px; min-width: 44px; }
.dxf-activities-dialog { max-width: calc(100vw - 2rem); max-height: calc(100dvh - 2rem); overflow-y: auto; }

@media (max-width: 768px) {
  .dxf-touch-target,
  .dxf-mobile-nav button {
    min-height: 44px;
    min-width: 44px;
  }
}

.app-bg-layer { position: fixed; inset: 0; z-index: 0; background-size: cover; background-position: center; pointer-events: none; }
.app-shell { position: relative; z-index: 1; height: 100%; width: 100%; }
body {
  background: var(--bg);
  color: var(--text);
  font-family: 'Space Grotesk', -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
}
.dxf-display { font-family: 'Bricolage Grotesque', 'Space Grotesk', sans-serif; letter-spacing: -0.015em; }
code, kbd, .dxf-mono, .font-mono { font-family: 'JetBrains Mono', ui-monospace, SFMono-Regular, Menlo, monospace; }
* { box-sizing: border-box; }
::-webkit-scrollbar { width: 10px; height: 10px; }
::-webkit-scrollbar-track { background: transparent; }
::-webkit-scrollbar-thumb { background: var(--border-strong); border-radius: 8px; transition: background 0.2s var(--ease); }
::-webkit-scrollbar-thumb:hover { background: color-mix(in srgb, var(--accent) 38%, transparent); }
button { cursor: pointer; }
button:disabled { cursor: not-allowed; }
.dxf-message-bookmark { display: inline-flex; align-items: center; justify-content: center; flex: 0 0 28px; width: 28px; height: 28px; padding: 0; line-height: 1; color: var(--text-muted); -webkit-app-region: no-drag; }
.dxf-message-bookmark span, .dxf-message-bookmark svg { display: block; width: 16px; height: 16px; pointer-events: none; }
.dxf-message-bookmark:hover, .dxf-message-bookmark[aria-pressed='true'] { color: var(--accent); border-color: var(--accent); }
.dxf-message-bookmark[aria-pressed='true'] { background-color: var(--accent-soft); }
.dxf-message-bookmark[aria-pressed='true'] svg { fill: currentColor; }
.dxf-message-bookmark:disabled { opacity: .45; }
.dxf-message-bookmark:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
input::placeholder { color: var(--text-dim); }

.dxf-cta {
  background-image: linear-gradient(100deg, var(--accent-strong), var(--accent));
  color: #1a1206; font-weight: 700; border: 1px solid transparent;
}
.dxf-cta:hover:not(:disabled) { filter: brightness(1.06); }
/* Not `opacity`: a faded gradient still reads as the live button. Off gets its
   own surface, so the eye stops offering it. */
.dxf-cta:disabled {
  background-image: none; background-color: var(--panel2);
  border-color: var(--border); color: var(--text-dim); font-weight: 600;
}
.dxf-wordmark { color: var(--text); }

.dxf-fold > summary { list-style: none; }
.dxf-fold > summary::-webkit-details-marker { display: none; }
.dxf-fold > summary::before { content: '▸'; margin-right: 7px; }
.dxf-fold[open] > summary::before { content: '▾'; }

.app-bg-pattern { position: fixed; inset: 0; z-index: 0; pointer-events: none; }
.app-bg-grid {
  background-color: var(--bg);
  background-image: linear-gradient(var(--edge) 1px, transparent 1px),
                    linear-gradient(90deg, var(--edge) 1px, transparent 1px);
  background-size: 28px 28px;
}
.app-bg-dots {
  background-color: var(--bg);
  background-image: radial-gradient(var(--edge) 1.4px, transparent 1.4px);
  background-size: 22px 22px;
}
.app-bg-aurora {
  background:
    radial-gradient(circle at 25% 30%, color-mix(in srgb, var(--accent) 22%, transparent), transparent 50%),
    radial-gradient(circle at 75% 70%, color-mix(in srgb, var(--violet) 18%, transparent), transparent 50%),
    var(--bg);
}
.app-bg-mesh {
  background-color: var(--bg);
  background-image:
    radial-gradient(circle at 20% 20%, var(--accent-soft), transparent 45%),
    radial-gradient(circle at 85% 80%, color-mix(in srgb, var(--violet) 10%, transparent), transparent 45%);
}
.app-bg-sunset {
  background:
    linear-gradient(160deg, color-mix(in srgb, var(--accent) 12%, var(--bg)), var(--bg) 60%),
    radial-gradient(circle at 70% 15%, color-mix(in srgb, var(--accent) 26%, transparent), transparent 45%);
}

/* `transform` is deliberately absent: drags are driven by transform through
   document::eval, and interpolating it would fight the pointer. */
button, a, input, textarea, select, summary, [role='button'] {
  transition: color 0.15s var(--ease),
              background-color 0.15s var(--ease),
              border-color 0.18s var(--ease),
              opacity 0.15s var(--ease);
}
button:active:not(:disabled) { transform: scale(0.985); }

.panel-hover {
  transition: border-color 0.2s var(--ease), background-color 0.2s var(--ease),
              box-shadow 0.25s var(--ease);
}
.panel-hover:hover {
  border-color: var(--border-strong);
  box-shadow: 0 0 10px -1px rgba(224, 160, 106, 0.16);
}

@keyframes dxf-fade-in {
  from { opacity: 0; transform: translateY(4px); }
  to   { opacity: 1; transform: translateY(0); }
}
.fade-in { animation: dxf-fade-in 0.18s var(--ease) both; }
.dxf-fade { animation: dxf-fade-in 0.2s var(--ease) both; }

.dx-spinner { width: 12px; height: 12px; border-radius: 50%; border: 2px solid rgba(255,255,255,0.12); border-top-color: var(--accent); display: inline-block; margin-right: 8px; animation: dxf-spin 1s linear infinite; }
@keyframes dxf-spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

/* Message arrival — a quick fade + slide-up as each new row mounts.

   `backwards`, not `both`, on purpose: `forwards` would leave the final
   `transform: translateY(0)` applied for the row's whole life, and a transform
   makes an element a stacking context *and* the containing block for any
   `position: fixed` descendant. That trapped the message hover-menu's z-index
   inside its own row (so the next row painted over the menu and swallowed the
   clicks) and would reduce a `fixed inset-0` dismiss layer to covering one row.
   The `to` state is identical to the row's unanimated style, so dropping it
   after the run is visually a no-op. */
@keyframes dxf-msg-in {
  from { opacity: 0; transform: translateY(5px); }
  to   { opacity: 1; transform: translateY(0); }
}
.dxf-msg-in { animation: dxf-msg-in 0.16s var(--ease) backwards; }

@keyframes dxf-pop {
  0%   { transform: scale(0.6); opacity: 0; }
  60%  { transform: scale(1.12); }
  100% { transform: scale(1); opacity: 1; }
}
.dxf-pop { animation: dxf-pop 0.18s var(--ease) both; }

@keyframes dxf-modal-in {
  0%   { opacity: 0; transform: scale(0.92) translateY(6px); }
  60%  { opacity: 1; transform: scale(1.02) translateY(0); }
  /* `none`, not scale(1): a transform left by `both` makes the dialog the box
     every `fixed` overlay inside it (the crop editor) is placed in. */
  100% { transform: none; }
}
.dxf-modal-in { animation: dxf-modal-in 0.22s cubic-bezier(0.34, 1.56, 0.64, 1) both; transform-origin: center; }
@keyframes dxf-backdrop-in { from { opacity: 0; } to { opacity: 1; } }
.dxf-backdrop-in { animation: dxf-backdrop-in 0.15s var(--ease) both; }

@keyframes dxf-pop-in { from { opacity: 0; transform: scale(0.96); } to { opacity: 1; transform: scale(1); } }
.dxf-pop-in { animation: dxf-pop-in 0.12s var(--ease) both; }

.dxf-composer { white-space: pre-wrap; overflow-wrap: anywhere; max-height: 10rem; overflow-x: hidden; overflow-y: auto; scrollbar-width: none; outline: none; cursor: text; }
.dxf-composer::-webkit-scrollbar { display: none; }
.dxf-composer:empty::before { content: attr(data-placeholder); color: var(--text-dim); pointer-events: none; }
.dxf-composer img { height: 1.5em; width: auto; display: inline-block; vertical-align: -0.4em; margin: 0 1px; }

.dxf-file-card { display: flex; align-items: center; flex-wrap: wrap; gap: 12px; width: min(100%, 28rem); box-sizing: border-box; margin-top: 8px; padding: 12px; border: 1px solid var(--border); border-radius: 12px; background-color: var(--panel2); }
.dxf-file-card:focus-within { border-color: var(--accent); }
.dxf-file-type { display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 4px; width: 44px; height: 52px; flex-shrink: 0; border-radius: 8px; background-color: var(--accent-soft); color: var(--accent); }
.dxf-file-type svg { display: block; width: 22px; height: 22px; }
.dxf-badge svg { display: block; width: 100%; height: 100%; }
.dxf-md { white-space: normal; }
.dxf-md > :first-child { margin-top: 0; }
.dxf-md p { margin: 0.15em 0; }
.dxf-md h1 { font-size: 1.4em; font-weight: 700; line-height: 1.25; margin: 0.4em 0 0.15em; }
.dxf-md h2 { font-size: 1.2em; font-weight: 700; line-height: 1.3; margin: 0.35em 0 0.1em; }
.dxf-md h3 { font-size: 1.05em; font-weight: 700; margin: 0.3em 0 0.1em; }
.dxf-md ul { list-style: disc; padding-left: 1.4em; margin: 0.15em 0; }
.dxf-md ol { list-style: decimal; padding-left: 1.6em; margin: 0.15em 0; }
.dxf-md blockquote { border-left: 3px solid var(--border-strong); padding-left: 0.6em; margin: 0.2em 0; color: var(--text-muted); }
.dxf-md code { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 0.9em; background-color: var(--panel2); border-radius: 4px; padding: 0.05em 0.3em; }
.dxf-md pre { background-color: var(--panel2); border: 1px solid var(--border); border-radius: 6px; padding: 0.5em 0.7em; margin: 0.25em 0; overflow-x: auto; }
.dxf-md pre code { background-color: transparent; padding: 0; white-space: pre; }
.dxf-md hr { border: 0; border-top: 1px solid var(--border); margin: 0.5em 0; }
.dxf-file-type > span:last-child { font-size: 9px; font-weight: 700; letter-spacing: 0.04em; }
.dxf-file-info { flex: 1 1 110px; min-width: 0; }
.dxf-file-name { color: var(--text); font-size: 13px; font-weight: 600; line-height: 1.4; overflow-wrap: anywhere; display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 2; overflow: hidden; }
.dxf-file-meta { display: flex; flex-wrap: wrap; gap: 4px 10px; margin-top: 4px; color: var(--text-dim); font-size: 11px; }
.dxf-file-download { display: inline-flex; align-items: center; justify-content: center; gap: 6px; flex-shrink: 0; margin-left: auto; padding: 8px 10px; border: 1px solid var(--border-strong); border-radius: 8px; color: var(--text-muted); background-color: var(--panel); font-size: 11px; font-weight: 600; }
.dxf-file-download svg { display: block; }
.dxf-file-download:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
.dxf-file-download:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
.dxf-file-download:disabled { opacity: 0.55; cursor: default; }
.dxf-file-card[data-expired=true] .dxf-file-type { color: var(--text-dim); background-color: var(--panel); }
.dxf-file-expired, .dxf-file-error { color: var(--danger); }
.dxf-file-error { flex-basis: 100%; margin: 0; font-size: 11px; overflow-wrap: anywhere; }

/* The connecting screen. Scanlines and a rolling band, because a dial that
   cannot report progress should at least look like it is doing something —
   there is no denominator here, only a wait. */
.dxf-crt {
  background-image: repeating-linear-gradient(
    to bottom,
    rgba(0,0,0,0.22) 0px, rgba(0,0,0,0.22) 1px,
    transparent 1px, transparent 3px);
  animation: dxf-crt-flicker 5.5s steps(1, end) infinite;
}
@keyframes dxf-crt-flicker {
  0%, 96%, 100% { opacity: 1; }
  97%           { opacity: 0.86; }
  98%           { opacity: 1; }
  99%           { opacity: 0.92; }
}
@keyframes dxf-crt-roll {
  from { transform: translateY(-30vh); }
  to   { transform: translateY(130vh); }
}
.dxf-crt-roll {
  height: 22vh;
  background: linear-gradient(to bottom, transparent,
    color-mix(in srgb, var(--accent) 9%, transparent), transparent);
  animation: dxf-crt-roll 3.6s linear infinite;
}
@keyframes dxf-connect-bob {
  0%, 100% { transform: translateY(0) scale(1); }
  50%      { transform: translateY(-14px) scale(1.04); }
}
.dxf-connect-bob { animation: dxf-connect-bob 1.5s var(--ease) infinite; }
@keyframes dxf-ellipsis { 0% { opacity: 0.25; } 50% { opacity: 1; } 100% { opacity: 0.25; } }
.dxf-ellipsis > span { animation: dxf-ellipsis 1.2s var(--ease) infinite; }
.dxf-ellipsis > span:nth-child(2) { animation-delay: 0.15s; }
.dxf-ellipsis > span:nth-child(3) { animation-delay: 0.3s; }

/* Fixed, and placed from the pointer: the guild rail scrolls, and an absolute
   tip beside a tile is clipped by the column that scrolls it. */
.dxf-tip {
  position: fixed; transform: translateY(-50%);
  z-index: 40; pointer-events: none; white-space: nowrap; max-width: 15rem; overflow: hidden;
  text-overflow: ellipsis; padding: 4px 8px; border-radius: 6px; font-size: 11px;
  background: var(--panel-solid); color: var(--text);
  border: 1px solid var(--border-strong); box-shadow: 0 6px 18px rgba(0,0,0,.45);
  opacity: 0; transition: opacity 0.12s var(--ease);
}
.dxf-tip-host:hover .dxf-tip { opacity: 1; transition-delay: 2s; }
/* Server rail strips explain a state, not a picture, so they answer sooner. */
.dxf-tip-host.dxf-tip-quick:hover .dxf-tip { transition-delay: .4s; }
.dxf-pending { animation: dxf-ellipsis 1.2s var(--ease) infinite; }

/* The update bar with no denominator: a silent installer reports nothing, so
   this says the process is alive, not how far along it is. */
@keyframes dxf-bar-slide { from { transform: translateX(-100%); } to { transform: translateX(400%); } }
.dxf-bar-slide { animation: dxf-bar-slide 1.1s cubic-bezier(0.4, 0, 0.2, 1) infinite; }

/* The dot sets `color` to its status colour so the currentColor ring matches. */
@keyframes dxf-dot-pulse {
  0%   { box-shadow: 0 0 0 0 currentColor; }
  70%  { box-shadow: 0 0 0 4px transparent; }
  100% { box-shadow: 0 0 0 0 transparent; }
}
.dxf-dot-pulse { animation: dxf-dot-pulse 2.4s var(--ease) infinite; }

/* macOS fullsize content view leaves no titlebar to grab. Controls inside a
   drag region must opt out with .dxf-no-drag or they stop being clickable. */
.dxf-drag-region { -webkit-app-region: drag; user-select: none; }
.dxf-no-drag { -webkit-app-region: no-drag; }

/* Inline SVG (see DiscordiaLogo) so the halves and splatter are real DOM
   nodes this stylesheet can drive. */
@keyframes dxf-logo-bob {
  0%, 100% { transform: translateY(0); }
  50%      { transform: translateY(-3%); }
}
.dxf-logo {
  display: inline-block;
  transform-origin: center;
  animation: dxf-logo-bob 6s var(--ease) infinite;
  transition: filter 0.4s var(--ease);
  will-change: transform, filter;
}
.dxf-logo svg { display: block; width: 100%; height: 100%; overflow: visible; }
.dxf-logo:hover {
  filter: drop-shadow(0 0 16px rgba(255, 210, 26, 0.55));
  animation-play-state: paused;
}

/* fill-box pins each rotation to that half's own centre; translates are in
   viewBox units (~1024 wide) so they scale with the logo. */
.dxf-logo-left, .dxf-logo-right {
  transform-box: fill-box;
  transform-origin: center;
}
@keyframes dxf-part-left {
  0%   { transform: translate(0, 0) rotate(0deg); }
  38%  { transform: translate(-130px, -58px) rotate(-15deg); }
  58%  { transform: translate(-130px, -58px) rotate(-15deg); }
  85%  { transform: translate(18px, 8px) rotate(2.5deg); }
  100% { transform: translate(0, 0) rotate(0deg); }
}
@keyframes dxf-part-right {
  0%   { transform: translate(0, 0) rotate(0deg); }
  38%  { transform: translate(130px, 58px) rotate(15deg); }
  58%  { transform: translate(130px, 58px) rotate(15deg); }
  85%  { transform: translate(-18px, -8px) rotate(-2.5deg); }
  100% { transform: translate(0, 0) rotate(0deg); }
}
.dxf-logo:hover .dxf-logo-left  { animation: dxf-part-left  0.95s cubic-bezier(0.65, 0, 0.35, 1) both; }
.dxf-logo:hover .dxf-logo-right { animation: dxf-part-right 0.95s cubic-bezier(0.65, 0, 0.35, 1) both; }

.dxf-splat {
  transform-box: view-box;
  transform-origin: 495px 515px;
  opacity: 0;
}
@keyframes dxf-splat-burst {
  0%, 76% { opacity: 0; transform: scale(0.1); }
  85%     { opacity: 0.95; transform: scale(0.8); }
  100%    { opacity: 0; transform: scale(1.55); }
}
.dxf-logo:hover .dxf-splat { animation: dxf-splat-burst 0.95s cubic-bezier(0.65, 0, 0.35, 1) both; }
";

pub struct ThemeDef {
    pub id: &'static str,
    pub label: &'static str,
    pub swatch: &'static str,
    vars: &'static str,
}

pub const THEMES: &[ThemeDef] = &[
    ThemeDef {
        id: "ember",
        label: "Ember",
        swatch: "#e8933a",
        vars: "--bg:#121110; --bg2:#1e1b17; --panel-solid:#17150f; --panel:#17150f; --panel2:#1b1813; \
               --edge:#262219; --edge-strong:#322c24; \
               --border:#2a2621; --border-strong:#3a342b; \
               --text:#f2ece3; --text-muted:#9a9287; --text-dim:#857d72; \
               --accent:#e8933a; --accent-soft:rgba(232,147,58,.16); --accent-strong:#f5ab5c; \
               --up:#6dbf6a; --success:#6dbf6a; --violet:#b98cff; --amber:#ffc46b; --warn:#ffc46b; --danger:#e8776a;",
    },
    ThemeDef {
        id: "midnight",
        label: "Midnight",
        swatch: "#6ea8ff",
        vars: "--bg:#080b12; --bg2:#0d1220; --panel-solid:#0c111c; --panel:#0c111c; --panel2:#111827; \
               --edge:rgba(110,168,255,.16); --edge-strong:rgba(110,168,255,.45); \
               --border:rgba(110,168,255,.16); --border-strong:rgba(110,168,255,.45); \
               --text:#e8eefc; --text-muted:#8b98b5; --text-dim:#586179; \
               --accent:#6ea8ff; --accent-soft:rgba(110,168,255,.13); --accent-strong:#9cc2ff; \
               --up:#5fe0c0; --success:#5fe0c0; --violet:#a99bff; --amber:#ffcf7a; --warn:#ffcf7a; --danger:#f2777a;",
    },
    ThemeDef {
        id: "violet",
        label: "Violet",
        swatch: "#c084fc",
        vars: "--bg:#100a16; --bg2:#180f22; --panel-solid:#160f1e; --panel:#160f1e; --panel2:#1d1428; \
               --edge:rgba(192,132,252,.16); --edge-strong:rgba(192,132,252,.45); \
               --border:rgba(192,132,252,.16); --border-strong:rgba(192,132,252,.45); \
               --text:#f2e9fb; --text-muted:#a495b8; --text-dim:#6a5c7c; \
               --accent:#c084fc; --accent-soft:rgba(192,132,252,.13); --accent-strong:#d3a6ff; \
               --up:#6ee7b7; --success:#6ee7b7; --violet:#ff9ed8; --amber:#ffc46b; --warn:#ffc46b; --danger:#f2777a;",
    },
    ThemeDef {
        id: "forest",
        label: "Forest",
        swatch: "#7bd88f",
        vars: "--bg:#0a0f0b; --bg2:#0f160f; --panel-solid:#0d130d; --panel:#0d130d; --panel2:#121a12; \
               --edge:rgba(123,216,143,.16); --edge-strong:rgba(123,216,143,.42); \
               --border:rgba(123,216,143,.16); --border-strong:rgba(123,216,143,.42); \
               --text:#e9f4ea; --text-muted:#93a894; --text-dim:#5c6b5d; \
               --accent:#7bd88f; --accent-soft:rgba(123,216,143,.13); --accent-strong:#a6e8b4; \
               --up:#7bd88f; --success:#7bd88f; --violet:#b98cff; --amber:#ffc46b; --warn:#ffc46b; --danger:#f2777a;",
    },
    ThemeDef {
        id: "daylight",
        label: "Day",
        swatch: "#e8730a",
        vars: "--bg:#f3ede2; --bg2:#ffffff; --panel-solid:#ffffff; --panel:#ffffff; --panel2:#f7f1e8; \
               --edge:rgba(180,120,60,.22); --edge-strong:rgba(120,80,40,.42); \
               --border:rgba(180,120,60,.22); --border-strong:rgba(120,80,40,.42); \
               --text:#2a2320; --text-muted:#6b615a; --text-dim:#a99e92; \
               --accent:#e8730a; --accent-soft:rgba(232,115,10,.12); --accent-strong:#c65f00; \
               --up:#2f9e6a; --success:#2f9e6a; --violet:#8b5cf6; --amber:#c77d1a; --warn:#c77d1a; --danger:#c0392b;",
    },
];

pub fn theme_vars(id: &str) -> &'static str {
    THEMES
        .iter()
        .find(|t| t.id == id)
        .map(|t| t.vars)
        .unwrap_or(THEMES[0].vars)
}

pub fn accent_vars(accent: &str) -> String {
    format!(
        "--accent: {accent}; --accent-strong: {accent}; \
         --accent-soft: color-mix(in srgb, {accent} 14%, transparent);"
    )
}

const FONT_SPACE_GROTESK: Asset = asset!("/assets/fonts/spacegrotesk.woff2");
const FONT_BRICOLAGE: Asset = asset!("/assets/fonts/bricolage.woff2");
const FONT_JETBRAINS_MONO: Asset = asset!("/assets/fonts/jetbrainsmono.woff2");

fn font_face_css() -> String {
    format!(
        "@font-face{{font-family:'Space Grotesk';font-style:normal;font-weight:300 700;\
           font-display:swap;src:url({sg}) format('woff2');}}\
         @font-face{{font-family:'Bricolage Grotesque';font-style:normal;font-weight:400 800;\
           font-display:swap;src:url({br}) format('woff2');}}\
         @font-face{{font-family:'JetBrains Mono';font-style:normal;font-weight:400 700;\
           font-display:swap;src:url({jb}) format('woff2');}}",
        sg = FONT_SPACE_GROTESK,
        br = FONT_BRICOLAGE,
        jb = FONT_JETBRAINS_MONO,
    )
}

pub(crate) fn desktop_config(language: crate::i18n::Language) -> crate::platform::Config {
    // Document styles are queued after DOM mutations; bootstrap CSS must precede the first frame.
    crate::platform::Config::new()
        .with_background_color((18, 17, 16, 255))
        .with_custom_head(format!(
            "<style>{TAILWIND_CSS}</style><style>{}</style><style>{BASE_CSS}</style>",
            font_face_css()
        ))
        .with_custom_index(desktop_index(language))
}

fn desktop_index(language: crate::i18n::Language) -> String {
    include_str!("../assets/desktop.html")
        .replace("__LANGUAGE__", language.tag())
        .replace("<!-- STARTUP_LOGO -->", include_str!("../assets/icon.svg"))
        .replace(
            "Loading Discordia…",
            &crate::i18n::translate(language, "Loading Discordia…", &[]),
        )
        .replace("/* STARTUP_SCRIPT */", include_str!("../assets/startup.js"))
}

/// The root keeps an opaque `--bg`; the shell above it does not.
///
/// The two layers exist for one reason: the pattern is painted on a fixed layer
/// *between* them, and `HomeView` and `WorkspaceView` both fill themselves with
/// `bg-[var(--bg)]`. Overriding `--bg` on the root — which is what the custom
/// image did — makes the pattern's own ground transparent too; overriding it on
/// the shell leaves the pattern something to sit on and takes the lid off it.
///
/// This is why every pattern did nothing at all before: the layer was drawn and
/// then painted over, every time.
fn shell_style(has_image: bool, pattern_class: &str) -> String {
    let mut out = String::new();
    if has_image || !pattern_class.is_empty() {
        out.push_str("--bg: transparent;");
    }
    // Panels stay solid for a pattern and go translucent for a photograph. Both
    // let the ground through where the app paints `--bg`, the chat log
    // included — a background nobody can see behind the biggest surface in the
    // window is the bug this function exists to fix.
    if has_image {
        out.push_str("--panel: color-mix(in srgb, var(--panel-solid) 66%, transparent);");
    }
    out
}

fn background_pattern_class(pattern: &str) -> &'static str {
    match pattern {
        "grid" => "app-bg-pattern app-bg-grid",
        "dots" => "app-bg-pattern app-bg-dots",
        "aurora" => "app-bg-pattern app-bg-aurora",
        "mesh" => "app-bg-pattern app-bg-mesh",
        "sunset" => "app-bg-pattern app-bg-sunset",
        _ => "",
    }
}

#[cfg(target_os = "windows")]
fn window_close_behavior(
    tray_icon_available: bool,
    tray_quit_available: bool,
) -> crate::platform::WindowCloseBehaviour {
    use crate::platform::WindowCloseBehaviour;

    if tray_icon_available && tray_quit_available {
        WindowCloseBehaviour::WindowHides
    } else {
        WindowCloseBehaviour::WindowCloses
    }
}

#[component]
pub fn App() -> Element {
    crate::mobile_back::use_navigation();
    let mut quitting = use_signal(|| false);
    use_context_provider(|| QuitRequest(quitting));
    let mut session = use_signal(|| None::<SessionParams>);
    let settings = use_signal(crate::settings::load_or_default);
    use_context_provider(|| settings);
    let locale =
        use_hook(move || Signal::new_in_scope(settings.peek().language.resolve(), ScopeId::ROOT));
    use_drop(move || locale.manually_drop());
    use_context_provider(|| crate::i18n::Locale(locale));
    let updates = crate::update::use_updates();
    let mut update_exit = use_signal(|| false);
    let quit_window = crate::platform::use_window();
    let close_to_tray = use_signal(|| false);
    let close_window = quit_window.clone();
    use_hook(move || {
        close_window.set_close_behavior(crate::platform::WindowCloseBehaviour::WindowHides)
    });
    let window_id = quit_window.window.id();
    crate::platform::use_wry_event_handler(move |event, _| {
        use crate::platform::tao::event::{Event, WindowEvent};
        if let Event::WindowEvent {
            window_id: id,
            event: WindowEvent::CloseRequested,
            ..
        } = event
            && *id == window_id
            && !*close_to_tray.peek()
        {
            quitting.set(true);
        }
    });
    use_effect(move || {
        let phase = updates.state.read().phase.clone();
        if matches!(phase, crate::update::UpdatePhase::Leaving(_)) {
            update_exit.set(true);
            if !*quitting.peek() {
                quitting.set(true);
            }
        }
        if *update_exit.peek()
            && matches!(
                phase,
                crate::update::UpdatePhase::Failed(_)
                    | crate::update::UpdatePhase::Opened
                    | crate::update::UpdatePhase::Ready(_)
            )
        {
            update_exit.set(false);
            quitting.set(false);
            return;
        }
        if quitting() && session.read().is_none() {
            if *update_exit.peek() && updates.prevents_close() {
                updates.install_after_disconnect();
                return;
            }
            quit_window.set_close_behavior(crate::platform::WindowCloseBehaviour::WindowCloses);
            quit_window.close();
        }
    });
    #[cfg(target_os = "windows")]
    {
        use crate::platform::{
            WindowCloseBehaviour,
            muda::{Menu, MenuItem},
            trayicon::TrayIconBuilder,
        };

        let tray_window = crate::platform::use_window();
        let tray_quit_available = TRAY_QUIT_AVAILABLE.load(std::sync::atomic::Ordering::SeqCst);
        use_future(move || {
            let receiver = TRAY_QUIT_RECEIVER
                .get()
                .and_then(|receiver| receiver.lock().ok())
                .and_then(|mut receiver| receiver.take());
            async move {
                if let Some(mut receiver) = receiver
                    && receiver.recv().await.is_some()
                {
                    quitting.set(true);
                }
            }
        });

        let tray_restore_window = tray_window.clone();
        use_future(move || {
            let receiver = TRAY_RESTORE_RECEIVER
                .get()
                .and_then(|receiver| receiver.lock().ok())
                .and_then(|mut receiver| receiver.take());
            let tray_window = tray_restore_window.clone();
            async move {
                if let Some(mut receiver) = receiver {
                    while let Some(()) = receiver.recv().await {
                        tray_window.set_close_behavior(WindowCloseBehaviour::WindowHides);
                        tray_window.set_visible(true);
                        tray_window.window.set_minimized(false);
                        tray_window.window.set_focus();
                    }
                }
            }
        });

        let tray_exit = use_hook(|| {
            MenuItem::with_id("discordia-quit", crate::i18n::label("Exit"), true, None)
        });
        let exit = tray_exit.clone();
        use_effect(move || tray_exit.set_text(crate::i18n::label("Exit")));
        let tray_icon = use_hook(move || {
            let menu = Menu::new();
            if let Err(error) = menu.append(&exit) {
                tracing::warn!("Could not create tray menu: {error}");
                return None;
            }

            let builder = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false);
            let icon = image::load_from_memory(include_bytes!("../assets/icon-1024.png"))
                .ok()
                .map(|image| {
                    let image = image
                        .resize(32, 32, image::imageops::FilterType::Lanczos3)
                        .to_rgba8();
                    let (width, height) = image.dimensions();
                    crate::platform::trayicon::Icon::from_rgba(image.into_raw(), width, height)
                });
            let builder = match icon {
                Some(Ok(icon)) => builder.with_icon(icon),
                Some(Err(error)) => {
                    tracing::warn!("Could not load tray icon: {error}");
                    builder
                }
                None => builder,
            };

            match builder.build() {
                Ok(icon) => Some(icon),
                Err(error) => {
                    tracing::warn!("Could not create tray icon: {error}");
                    None
                }
            }
        });

        use_effect(move || {
            let mut close_to_tray = close_to_tray;
            close_to_tray.set(
                window_close_behavior(tray_icon.is_some(), tray_quit_available)
                    == WindowCloseBehaviour::WindowHides,
            );
        });
    }

    let mut identity = use_signal(|| {
        let identity = Identity::load().ok().flatten();
        // Bind the old shared cache before sign-in can select a different account.
        if let Some(identity) = &identity
            && let Err(error) = crate::profile::migrate_legacy(&identity.pubkey)
        {
            tracing::warn!(%error, "could not migrate the account profile");
        }
        identity
    });
    let mut update_dialog = use_signal(|| false);
    use_effect(move || {
        let request = *updates.open_request.read();
        if request > 0 && identity.peek().is_none() {
            update_dialog.set(true);
        }
    });
    let mut error = use_signal(|| None::<String>);
    let last_session = use_signal(|| session::load().ok().flatten());

    let appearance = settings.read();
    let theme = appearance.theme.clone();
    let size_css = crate::ui_size::text_css(appearance.text_size_percent);
    let accent = appearance.accent.clone();
    let background = appearance.background.clone();
    let pattern = appearance.pattern.clone();
    let scrim = (appearance.background_dim.min(95) as f64) / 100.0;
    drop(appearance);
    let pattern_class = if background.is_some() {
        ""
    } else {
        background_pattern_class(&pattern)
    };

    let mut root_style = theme_vars(&theme).to_string();
    if let Some(a) = &accent {
        root_style.push_str(&accent_vars(a));
    }
    let shell = shell_style(background.is_some(), pattern_class);
    let platform_class = if cfg!(any(target_os = "android", target_os = "ios")) {
        "dxf-platform-mobile"
    } else {
        ""
    };

    rsx! {
        AppHead {}
        style { {include_str!("../assets/updates.css")} }
        style { {include_str!("../assets/mobile.css")} }
        style { "{size_css}" }

        div {
            class: "dxf-ui {platform_class} h-screen w-screen bg-[var(--bg)] text-[var(--text)] antialiased overflow-hidden",
            style: "{root_style}; display:flex; flex-direction:column;",
            if !pattern_class.is_empty() {
                div { class: "{pattern_class}" }
            }
            if let Some(img) = background {
                div { class: "app-bg-layer", style: "background-image: url('{img}');" }
                div { class: "app-bg-layer", style: "background: rgba(0,0,0,{scrim});" }
            }
            crate::update::UpdateBanner {}
            if identity.read().is_none() && update_dialog() {
                div { class: "fixed inset-0 z-[70] flex items-center justify-center bg-black/50 p-6",
                    onclick: move |_| update_dialog.set(false),
                    div { class: "dxf-update-dialog", role: "dialog", aria_modal: "true", aria_label: crate::i18n::text("Updates", &[]),
                        onclick: move |event: MouseEvent| event.stop_propagation(),
                        button { class: "dxf-update-dismiss", aria_label: crate::i18n::text("Close", &[]), onclick: move |_| update_dialog.set(false), "×" }
                        crate::update::UpdatePanel {}
                    }
                }
            }
            div { class: "app-shell", style: "{shell}; flex:1; min-height:0; height:auto;",
            match (identity.read().clone(), session.read().clone()) {
                (None, _) => rsx! {
                    IdentitySetupView {
                        on_done: move |new_id: Identity| identity.set(Some(new_id)),
                    }
                },
                (Some(id), active_session) => rsx! {
                    for id in std::iter::once(id) {
                    AccountView {
                        key: "{id.pubkey}",
                        identity: id,
                        session: active_session.clone(),
                        error: error(),
                        last_session: last_session.read().clone(),
                        on_connect: move |params: SessionParams| {
                            error.set(None);
                            let saved = SavedSession {
                                mode: params.mode.clone(),
                            };
                            let _ = session::save(&saved);
                            session.set(Some(params));
                        },
                        on_rename: move |new_name: String| {
                            let current = identity.peek().clone();
                            if let Some(mut renamed) = current {
                                renamed.set_display_name(crate::protocol::canonical_username(&new_name))?;
                                identity.set(Some(renamed));
                                error.set(None);
                                Ok(())
                            } else {
                                Err(crate::i18n::text("No account is signed in.", &[]))
                            }
                        },
                        on_sign_out: move |_| {
                            let _ = crate::identity::sign_out();
                            let _ = session::clear();
                            session.set(None);
                            identity.set(None);
                            if !quitting() {
                                updates.check();
                            }
                        },
                        on_disconnect: move |reason: String| {
                            error.set(if reason.is_empty() { None } else { Some(reason) });
                            session.set(None);
                            if !quitting() {
                                updates.check();
                            }
                        },
                    }
                    }
                },
            }
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct QuitRequest(pub Signal<bool>);

pub(crate) fn session_key(p: &SessionParams) -> String {
    let mode = match &p.mode {
        SessionMode::Remote { server_url } => format!("remote:{server_url}"),
        SessionMode::SelfHost {
            allow_lan,
            manual_ip,
            rendezvous_url,
            publish_public,
            ..
        } => {
            format!(
                "selfhost:{allow_lan}:{}:{publish_public}:{manual_ip:?}",
                rendezvous_url.as_deref().unwrap_or("")
            )
        }
        SessionMode::ByCode {
            rendezvous_url,
            code,
            ..
        } => {
            format!("bycode:{rendezvous_url}:{code}")
        }
    };
    format!("{mode}|{}|{}", p.identity.display_name, p.identity.pubkey)
}

#[component]
/// Prop-less so Dioxus memoizes it — re-evaluating it warns "Changing the
/// props of Style/Script is not supported".
pub(crate) fn AppHead() -> Element {
    crate::image_assets::use_image_assets();
    rsx! {
        document::Style { {include_str!("../assets/stream-viewer.css")} }
        document::Style { {include_str!("../assets/accent-palette.css")} }
        document::Style { {include_str!("../assets/guild-hub.css")} }
        document::Style { {include_str!("../assets/text-menu.css")} }
        document::Style { {include_str!("../assets/image-viewer.css")} }
        document::Style { {include_str!("../assets/voice-drag.css")} }
        crate::text_menu::TextMenu {}
        crate::i18n::LanguageDocument {}
        document::Script { {LIVEKIT_JS} }
        document::Script { {include_str!("../assets/gif-playback.js")} }
        document::Script { {include_str!("../assets/activity-clock.js")} }
        document::Script {
            {format!(
                "window.__dxfE2eeWorkerSrc = {};",
                serde_json::to_string(&format!("{LIVEKIT_E2EE_WORKER_JS}\n{E2EE_WORKER_SHIM_JS}"))
                    .unwrap_or_else(|_| "null".into())
            )}
        }
        document::Script { {crate::features::screenshare::SCREEN_JS} }
    }
}

#[cfg(test)]
mod tests {
    use super::openable;

    #[test]
    fn startup_document_uses_the_selected_language_before_mounting() {
        use crate::i18n::Language;
        for (language, tag, loading) in [
            (Language::English, "en", "Loading Discordia…"),
            (Language::Spanish, "es", "Cargando Discordia…"),
            (Language::Portuguese, "pt-BR", "Carregando Discordia…"),
            (Language::ChineseSimplified, "zh-CN", "正在加载 Discordia…"),
            (Language::Japanese, "ja", "Discordia を読み込み中…"),
        ] {
            let html = super::desktop_index(language);
            assert!(html.contains(&format!("<html lang=\"{tag}\">")));
            assert!(html.contains(&format!("<span>{loading}</span>")));
            assert!(!html.contains("/* STARTUP_SCRIPT */"));
            assert!(!html.contains("<!-- STARTUP_LOGO -->"));
            assert!(html.contains("viewBox=\"0 0 1024 1024\""));
        }
    }

    #[cfg(target_os = "windows")]
    use super::window_close_behavior;

    #[cfg(target_os = "windows")]
    #[test]
    fn tray_close_behavior_hides_window_when_tray_support_is_ready() {
        let behavior = window_close_behavior(true, true);
        assert!(matches!(
            behavior,
            crate::platform::WindowCloseBehaviour::WindowHides
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn tray_close_behavior_closes_window_when_tray_support_is_missing() {
        let behavior = window_close_behavior(false, true);
        assert!(matches!(
            behavior,
            crate::platform::WindowCloseBehaviour::WindowCloses
        ));
    }

    #[test]
    fn rejects_non_web_schemes() {
        assert!(openable("javascript:alert(1)").is_none());
        assert!(openable("file:///etc/passwd").is_none());
    }

    #[test]
    fn rejects_http_without_host() {
        assert!(openable("http://").is_none());
    }

    #[test]
    fn keeps_shell_metacharacters_as_url_text() {
        let url = openable("https://a.b/?x&calc").expect("a well-formed https URL");
        assert_eq!(url.host_str(), Some("a.b"));
        assert_eq!(url.query(), Some("x&calc"));
    }

    #[test]
    fn round_trips_a_plain_url() {
        assert_eq!(
            openable("https://example.com/path?q=1").map(|u| u.to_string()),
            Some("https://example.com/path?q=1".to_string())
        );
    }
}

#[cfg(test)]
mod background_tests {
    use super::{background_pattern_class, shell_style};

    #[test]
    fn a_pattern_lifts_the_lid_that_was_hiding_it() {
        let cls = background_pattern_class("dots");
        assert!(!cls.is_empty());
        let shell = shell_style(false, cls);
        assert!(
            shell.contains("--bg: transparent"),
            "the shell has to stop painting over the pattern layer"
        );
        assert!(
            !shell.contains("--panel"),
            "panels stay solid for a pattern; only a photograph earns translucency"
        );
    }

    #[test]
    fn no_pattern_and_no_image_leaves_every_surface_alone() {
        assert_eq!(background_pattern_class("none"), "");
        assert_eq!(shell_style(false, ""), "");
    }

    #[test]
    fn an_image_still_gets_translucent_panels() {
        let shell = shell_style(true, "");
        assert!(shell.contains("--bg: transparent"));
        assert!(shell.contains("--panel"));
    }

    #[test]
    fn every_offered_pattern_resolves_to_a_layer() {
        for (id, _, _) in crate::features::appearance::BACKGROUND_TILES {
            let cls = background_pattern_class(id);
            if *id == "none" {
                assert!(cls.is_empty());
            } else {
                assert!(cls.starts_with("app-bg-pattern "), "{id} draws nothing");
            }
        }
    }

    /// `--bg` is overridden to `transparent` whenever a pattern or an image is
    /// on, so anything using it as *ink* disappears. Two unread badges did.
    #[test]
    fn nothing_paints_foreground_with_the_overridable_ground() {
        for file in [
            include_str!("features/guilds.rs"),
            include_str!("features/channels.rs"),
            include_str!("features/chat.rs"),
            include_str!("features/members.rs"),
            include_str!("features/workspace.rs"),
        ] {
            assert!(
                !file.contains("text-[var(--bg)]"),
                "use --panel-solid for ink: --bg goes transparent under a background"
            );
        }
    }
}
