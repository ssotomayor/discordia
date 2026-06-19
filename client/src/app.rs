use dioxus::prelude::*;

use crate::features::{
    connect::ConnectView, identity_setup::IdentitySetupView, workspace::WorkspaceView,
};
use crate::identity::Identity;
use crate::session::{self, SavedSession};
use crate::state::{SessionMode, SessionParams};

/// App brand mark, inlined as raw SVG markup. We render it inline (rather
/// than via an `<img>` asset) so the two halves and the splatter layer are
/// real DOM nodes the stylesheet can animate independently on hover — an
/// `<img>` exposes none of its internals to CSS. The `.dxf-logo-left`,
/// `.dxf-logo-right` and `.dxf-splat` classes are the animation handles
/// (see BASE_CSS). Gradient/filter ids are prefixed `dxf` to avoid clashing
/// with anything else on the page.
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

/// Start a native window drag. wry/WKWebView does NOT honour Electron's
/// `-webkit-app-region: drag` CSS, so our `.dxf-drag-region` strips can't move
/// the window on their own — they need an explicit `onmousedown` that asks tao
/// to drag. Interactive children must `stop_propagation()` so clicks/selection
/// still work (the CSS equivalent of `-webkit-app-region: no-drag`).
pub fn start_window_drag() {
    dioxus::desktop::window().drag();
}

/// Open a URL in the user's real browser (not the in-app webview, which would
/// navigate away from the app). Best-effort, per-platform.
pub fn open_external(url: &str) {
    let url = url.to_string();
    #[cfg(target_os = "macos")]
    let cmd = ("open", vec![url]);
    #[cfg(target_os = "windows")]
    let cmd = ("cmd", vec!["/C".to_string(), "start".to_string(), url]);
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let cmd = ("xdg-open", vec![url]);
    let _ = std::process::Command::new(cmd.0).args(cmd.1).spawn();
}

/// Inline Discordia brand mark. `class` sets the size (e.g. "w-32 h-32").
#[component]
pub fn DiscordiaLogo(#[props(into)] class: String) -> Element {
    rsx! {
        div {
            class: "dxf-logo {class}",
            dangerous_inner_html: DISCORDIA_LOGO_SVG,
        }
    }
}

const BASE_CSS: &str = "
:root {
  --bg: #0a0908;
  --panel: #0a0908;
  --border: rgba(228, 105, 23, 0.41);
  --border-strong: rgba(255, 209, 179, 0.35);
  --text: #d6d6d6;
  --text-muted: #888888;
  --text-dim: #5a5a5a;
  --accent: #e0a06a;
  --accent-soft: rgba(224, 160, 106, 0.10);
  --accent-strong: #ec8f3f;
  --success: #8fa872;
  --warn: #d4a04f;
  --danger: #c67878;
  --ease: cubic-bezier(0.4, 0.0, 0.2, 1);
}
html, body, #main { height: 100%; margin: 0; }

/* Optional local background image: two fixed layers (image + darkening
   scrim) behind a relatively-positioned app shell. When a background is set,
   the root's inline vars make the app backdrop transparent and panels
   translucent (see App) so the image shows through. */
.app-bg-layer { position: fixed; inset: 0; z-index: 0; background-size: cover; background-position: center; pointer-events: none; }
.app-shell { position: relative; z-index: 1; height: 100%; width: 100%; }
body {
  background: var(--bg);
  color: var(--text);
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
}
* { box-sizing: border-box; }
::-webkit-scrollbar { width: 8px; height: 8px; }
::-webkit-scrollbar-track { background: transparent; }
::-webkit-scrollbar-thumb { background: rgba(190, 130, 90, 0.18); border-radius: 4px; transition: background 0.2s var(--ease); }
::-webkit-scrollbar-thumb:hover { background: rgba(190, 130, 90, 0.35); }
button { cursor: pointer; }
button:disabled { cursor: not-allowed; }
input::placeholder { color: var(--text-dim); }

/* Smooth color/border transitions on every interactive surface. Excluded
   from `transform` so drag-in-progress (which is driven by transform via
   document::eval) doesn't get interpolated. */
button, a, input, textarea, select, summary, [role='button'] {
  transition: color 0.15s var(--ease),
              background-color 0.15s var(--ease),
              border-color 0.18s var(--ease),
              opacity 0.15s var(--ease);
}
button:active:not(:disabled) { transform: scale(0.985); }

/* Apply to any bordered panel/widget for a subtle hover brightening plus a
   very slight warm glow bleeding off the border. */
.panel-hover {
  transition: border-color 0.2s var(--ease), background-color 0.2s var(--ease),
              box-shadow 0.25s var(--ease);
}
.panel-hover:hover {
  border-color: var(--border-strong);
  box-shadow: 0 0 10px -1px rgba(224, 160, 106, 0.16);
}

/* Fade-in animation used on tab content / step content so switches feel
   intentional instead of jarring snaps. */
@keyframes dxf-fade-in {
  from { opacity: 0; transform: translateY(4px); }
  to   { opacity: 1; transform: translateY(0); }
}
.fade-in { animation: dxf-fade-in 0.18s var(--ease) both; }
.dxf-fade { animation: dxf-fade-in 0.2s var(--ease) both; }

/* Message arrival — a quick fade + slide-up as each new row mounts. */
@keyframes dxf-msg-in {
  from { opacity: 0; transform: translateY(5px); }
  to   { opacity: 1; transform: translateY(0); }
}
.dxf-msg-in { animation: dxf-msg-in 0.16s var(--ease) both; }

/* Pop — reaction chips and badges springing in. */
@keyframes dxf-pop {
  0%   { transform: scale(0.6); opacity: 0; }
  60%  { transform: scale(1.12); }
  100% { transform: scale(1); opacity: 1; }
}
.dxf-pop { animation: dxf-pop 0.18s var(--ease) both; }

/* Online presence — a soft expanding ring. The dot sets `color` to its
   status color so the ring (currentColor) matches. */
@keyframes dxf-dot-pulse {
  0%   { box-shadow: 0 0 0 0 currentColor; }
  70%  { box-shadow: 0 0 0 4px transparent; }
  100% { box-shadow: 0 0 0 0 transparent; }
}
.dxf-dot-pulse { animation: dxf-dot-pulse 2.4s var(--ease) infinite; }

/* Window drag regions. With macOS fullsize content view + transparent
   titlebar, there's no OS titlebar strip — so the user needs SOME region
   they can grab to move the window. Anything tagged .dxf-drag-region is
   draggable; buttons / inputs inside such a region must opt out with
   .dxf-no-drag so they remain clickable. The traffic lights stay at
   the top-left and float over our content. */
.dxf-drag-region { -webkit-app-region: drag; user-select: none; }
.dxf-no-drag { -webkit-app-region: no-drag; }

/* Discordia brand mark. Rendered as INLINE svg (see DiscordiaLogo) so the
   two halves + the splatter layer are real DOM nodes we can drive here.
   At rest: a near-imperceptible bob so it feels alive without nagging.
   On hover: the halves swing apart along the seam, hang for a beat, then
   slam back together with a slight overshoot while a burst of paint specks
   splatters out of the seam and fades. Eased in-out throughout. */
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

/* The two halves split along the diagonal seam. transform-box: fill-box pins
   each rotation to that half's own centre; translate values are in viewBox
   user units (the artwork is ~1024 wide), so they scale with the logo. */
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

/* Paint splatter — hidden until the halves slam back together, then a quick
   burst that radiates out of the seam and fades. */
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

/// A selectable color theme: a set of CSS custom-property overrides. `--ease`
/// and structural rules stay in BASE_CSS; themes only restyle colors.
pub struct ThemeDef {
    pub id: &'static str,
    pub label: &'static str,
    /// A swatch color for the picker (the accent).
    pub swatch: &'static str,
    vars: &'static str,
}

/// Available themes. The first ("ember") matches the original look.
pub const THEMES: &[ThemeDef] = &[
    ThemeDef {
        id: "ember",
        label: "Ember",
        swatch: "#e0a06a",
        vars: "--bg:#0a0908; --panel-solid:#0a0908; --panel:var(--panel-solid); \
               --border:rgba(228,105,23,0.41); --border-strong:rgba(255,209,179,0.35); \
               --text:#d6d6d6; --text-muted:#888888; --text-dim:#5a5a5a; \
               --accent:#e0a06a; --accent-soft:rgba(224,160,106,0.10); --accent-strong:#ec8f3f; \
               --success:#8fa872; --warn:#d4a04f; --danger:#c67878;",
    },
    ThemeDef {
        id: "midnight",
        label: "Midnight",
        swatch: "#6c8cff",
        vars: "--bg:#0a0e1a; --panel-solid:#0c1120; --panel:var(--panel-solid); \
               --border:rgba(108,140,255,0.34); --border-strong:rgba(170,190,255,0.40); \
               --text:#d8def0; --text-muted:#8590ad; --text-dim:#586079; \
               --accent:#6c8cff; --accent-soft:rgba(108,140,255,0.12); --accent-strong:#5a7cff; \
               --success:#7fb0a0; --warn:#d4b25a; --danger:#d07a8a;",
    },
    ThemeDef {
        id: "forest",
        label: "Forest",
        swatch: "#6fbf8a",
        vars: "--bg:#0a0f0c; --panel-solid:#0c130e; --panel:var(--panel-solid); \
               --border:rgba(111,191,138,0.32); --border-strong:rgba(180,230,200,0.38); \
               --text:#d6e0d8; --text-muted:#82917f; --text-dim:#566054; \
               --accent:#6fbf8a; --accent-soft:rgba(111,191,138,0.12); --accent-strong:#57b277; \
               --success:#8fc89a; --warn:#cdb45c; --danger:#c98080;",
    },
    ThemeDef {
        id: "rose",
        label: "Rose",
        swatch: "#e08ab0",
        vars: "--bg:#140a10; --panel-solid:#180b14; --panel:var(--panel-solid); \
               --border:rgba(224,138,176,0.33); --border-strong:rgba(255,200,224,0.40); \
               --text:#ecd9e3; --text-muted:#a3899a; --text-dim:#6e5a66; \
               --accent:#e08ab0; --accent-soft:rgba(224,138,176,0.12); --accent-strong:#ec79a8; \
               --success:#9fc090; --warn:#d6ab5e; --danger:#e0788a;",
    },
    ThemeDef {
        id: "mono",
        label: "Mono",
        swatch: "#c8c8cc",
        vars: "--bg:#0c0c0d; --panel-solid:#101012; --panel:var(--panel-solid); \
               --border:rgba(200,200,210,0.22); --border-strong:rgba(230,230,240,0.34); \
               --text:#dcdce0; --text-muted:#86868c; --text-dim:#56565c; \
               --accent:#c8c8cc; --accent-soft:rgba(200,200,210,0.08); --accent-strong:#e6e6ea; \
               --success:#9bb89b; --warn:#ccb96a; --danger:#cc8a8a;",
    },
    ThemeDef {
        id: "daylight",
        label: "Daylight",
        swatch: "#c2703a",
        vars: "--bg:#f4f2ee; --panel-solid:#ffffff; --panel:var(--panel-solid); \
               --border:rgba(180,120,60,0.30); --border-strong:rgba(120,80,40,0.42); \
               --text:#2a2724; --text-muted:#6b665f; --text-dim:#a39c92; \
               --accent:#c2703a; --accent-soft:rgba(194,112,58,0.12); --accent-strong:#a85a28; \
               --success:#5a8a4a; --warn:#b07a20; --danger:#b85040;",
    },
];

/// The raw CSS custom-property declarations for a theme id. Applied as an
/// inline `style` on the app root (deterministic + reactive — far more reliable
/// than swapping a `<style>` block, whose `:root` can lose to BASE_CSS).
pub fn theme_vars(id: &str) -> &'static str {
    THEMES
        .iter()
        .find(|t| t.id == id)
        .map(|t| t.vars)
        .unwrap_or(THEMES[0].vars)
}

/// CSS variable declarations for an accent-color override (layered on a theme).
pub fn accent_vars(accent: &str) -> String {
    format!(
        "--accent: {accent}; --accent-strong: {accent}; \
         --accent-soft: color-mix(in srgb, {accent} 14%, transparent);"
    )
}

#[component]
pub fn App() -> Element {
    let mut identity = use_signal(|| Identity::load().ok().flatten());
    let mut session = use_signal(|| None::<SessionParams>);
    let mut error = use_signal(|| None::<String>);
    let last_session = use_signal(|| session::load().ok().flatten());

    // Local appearance settings (theme + background). Shared via context so
    // the in-app Appearance panel can mutate them live.
    let settings = use_signal(crate::settings::load_or_default);
    use_context_provider(|| settings);
    let appearance = settings.read();
    let theme = appearance.theme.clone();
    let accent = appearance.accent.clone();
    let background = appearance.background.clone();
    let scrim = (appearance.background_dim.min(95) as f64) / 100.0;
    drop(appearance);

    // Theme + accent + background are applied as inline CSS variables on the
    // root element so they cascade to everything and update reactively.
    let mut root_style = theme_vars(&theme).to_string();
    if let Some(a) = &accent {
        root_style.push_str(&accent_vars(a));
    }
    if background.is_some() {
        // Let the background show through: transparent backdrop + translucent
        // panels (no blur — backdrop-filter would trap fixed-position modals).
        root_style.push_str(
            "--bg: transparent; --panel: color-mix(in srgb, var(--panel-solid) 66%, transparent);",
        );
    }

    rsx! {
        document::Script { src: "https://unpkg.com/@tailwindcss/browser@4" }
        document::Style { {BASE_CSS} }

        div {
            class: "h-screen w-screen bg-[var(--bg)] text-[var(--text)] antialiased overflow-hidden",
            style: "{root_style}",
            if let Some(img) = background {
                div { class: "app-bg-layer", style: "background-image: url('{img}');" }
                div { class: "app-bg-layer", style: "background: rgba(0,0,0,{scrim});" }
            }
            div { class: "app-shell",
            match (identity.read().clone(), session.read().clone()) {
                (None, _) => rsx! {
                    IdentitySetupView {
                        on_done: move |new_id: Identity| identity.set(Some(new_id)),
                    }
                },
                (Some(id), None) => rsx! {
                    ConnectView {
                        identity: id,
                        error: error(),
                        last_session: last_session.read().clone(),
                        on_connect: move |params: SessionParams| {
                            error.set(None);
                            // Persist for next launch's Reconnect button.
                            let saved = SavedSession {
                                mode: params.mode.clone(),
                                username: params.username.clone(),
                            };
                            let _ = session::save(&saved);
                            session.set(Some(params));
                        },
                        on_rename: move |new_name: String| {
                            // Mutate the live identity + persist to disk. The
                            // new name takes effect on the next Connect (we
                            // don't surgery the in-flight gateway session).
                            let mut current = identity.write();
                            if let Some(id) = current.as_mut() {
                                let _ = id.set_display_name(new_name);
                            }
                        },
                        on_sign_out: move |_| {
                            let _ = Identity::delete_file();
                            let _ = session::clear();
                            session.set(None);
                            identity.set(None);
                        },
                    }
                },
                (Some(_), Some(params)) => rsx! {
                    WorkspaceView {
                        key: "{session_key(&params)}",
                        params: params.clone(),
                        on_disconnect: move |reason: String| {
                            // An empty reason means the user deliberately
                            // unplugged — return to the connect screen
                            // without flagging it as an error.
                            error.set(if reason.is_empty() { None } else { Some(reason) });
                            session.set(None);
                        },
                    }
                },
            }
            }
        }
    }
}

fn session_key(p: &SessionParams) -> String {
    let mode = match &p.mode {
        SessionMode::Remote { server_url } => format!("remote:{server_url}"),
        SessionMode::SelfHost { allow_lan, rendezvous_url, publish_public, .. } => {
            format!(
                "selfhost:{allow_lan}:{}:{publish_public}",
                rendezvous_url.as_deref().unwrap_or("")
            )
        }
        SessionMode::ByCode { rendezvous_url, code } => {
            format!("bycode:{rendezvous_url}:{code}")
        }
    };
    format!("{mode}|{}|{}", p.username, p.identity.pubkey)
}
