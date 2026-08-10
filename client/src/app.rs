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

/// Tailwind utility classes, generated at build time from `assets/tailwind.css`
/// by `npx @tailwindcss/cli`. Inlined into the binary via `include_str!()` so
/// it works with both `cargo run` and `dx serve` — `asset!()` requires the `dx`
/// CLI as a custom linker to process assets, which breaks `cargo run`. This
/// renders a `<style>` tag with the full CSS in the `<head>`, same as
/// `BASE_CSS` and `font_face_css()`. No CDN, no runtime compiler, no FOUC,
/// works offline.
const TAILWIND_CSS: &str = include_str!("../assets/tailwind.out.css");

const BASE_CSS: &str = "
/* Default (ember) palette. Per-theme overrides are applied inline on the app
   root by `theme_vars()`. The existing variable *names* are kept as the
   styling interface so components need no churn; the design's richer palette
   (bg2/panel2/up/violet/amber) is layered on as additional vars. */
:root {
  --bg: #0e0b08; --bg2: #171017;
  --panel-solid: #17110c; --panel: #17110c; --panel2: #1e160f;
  --edge: rgba(255,158,61,.15); --edge-strong: rgba(255,158,61,.42);
  --border: rgba(255,158,61,.15); --border-strong: rgba(255,158,61,.42);
  --text: #f4ece2; --text-muted: #a8988a; --text-dim: #6c5f53;
  --accent: #ff9e3d; --accent-soft: rgba(255,158,61,.13); --accent-strong: #ffb26b;
  --up: #5fe0a8; --success: #5fe0a8;
  --violet: #b98cff; --amber: #ffc46b; --warn: #ffc46b;
  --danger: #f2777a;
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
  font-family: 'Space Grotesk', -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
}
/* Display face for the wordmark + headings; mono face for keys/codes. */
.dxf-display { font-family: 'Bricolage Grotesque', 'Space Grotesk', sans-serif; letter-spacing: -0.015em; }
code, kbd, .dxf-mono, .font-mono { font-family: 'JetBrains Mono', ui-monospace, SFMono-Regular, Menlo, monospace; }
* { box-sizing: border-box; }
::-webkit-scrollbar { width: 8px; height: 8px; }
::-webkit-scrollbar-track { background: transparent; }
::-webkit-scrollbar-thumb { background: color-mix(in srgb, var(--accent) 20%, transparent); border-radius: 4px; transition: background 0.2s var(--ease); }
::-webkit-scrollbar-thumb:hover { background: color-mix(in srgb, var(--accent) 38%, transparent); }
button { cursor: pointer; }
button:disabled { cursor: not-allowed; }
input::placeholder { color: var(--text-dim); }

/* Gradient CTA (blue→accent, the primary action buttons in the design) and the
   gradient wordmark treatment. */
.dxf-cta {
  background-image: linear-gradient(100deg, #8fb0ff, var(--accent));
  color: #0e0b08; font-weight: 600; border: none;
  box-shadow: 0 0 24px -6px color-mix(in srgb, var(--accent) 55%, transparent);
}
.dxf-cta:hover { filter: brightness(1.06); }
.dxf-wordmark {
  background-image: linear-gradient(105deg, #f4ece2 0%, #e9d9c2 30%, #8fb0ff 62%, var(--accent) 100%);
  -webkit-background-clip: text; background-clip: text;
  -webkit-text-fill-color: transparent; color: transparent;
}

/* Procedural app backgrounds (selectable in the theme popover). The layer
   sits behind .app-shell (z-0). Only one is active at a time via a class on
   .app-bg-pattern. */
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
  background:
    var(--bg),
    radial-gradient(circle at 20% 20%, var(--accent-soft), transparent 45%),
    radial-gradient(circle at 85% 80%, color-mix(in srgb, var(--violet) 10%, transparent), transparent 45%);
}
.app-bg-sunset {
  background:
    linear-gradient(160deg, color-mix(in srgb, var(--accent) 12%, var(--bg)), var(--bg) 60%),
    radial-gradient(circle at 70% 15%, color-mix(in srgb, var(--accent) 26%, transparent), transparent 45%);
}

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

/* Small spinner used in popovers for reconnection state. */
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

/* Pop — reaction chips and badges springing in. */
@keyframes dxf-pop {
  0%   { transform: scale(0.6); opacity: 0; }
  60%  { transform: scale(1.12); }
  100% { transform: scale(1); opacity: 1; }
}
.dxf-pop { animation: dxf-pop 0.18s var(--ease) both; }

/* Dialogs zoom in with a little overshoot bounce; their backdrop fades. */
@keyframes dxf-modal-in {
  0%   { opacity: 0; transform: scale(0.92) translateY(6px); }
  60%  { opacity: 1; transform: scale(1.02) translateY(0); }
  100% { transform: scale(1); }
}
.dxf-modal-in { animation: dxf-modal-in 0.22s cubic-bezier(0.34, 1.56, 0.64, 1) both; transform-origin: center; }
@keyframes dxf-backdrop-in { from { opacity: 0; } to { opacity: 1; } }
.dxf-backdrop-in { animation: dxf-backdrop-in 0.15s var(--ease) both; }

/* Lighter, quicker scale for small popovers/menus. */
@keyframes dxf-pop-in { from { opacity: 0; transform: scale(0.96); } to { opacity: 1; transform: scale(1); } }
.dxf-pop-in { animation: dxf-pop-in 0.12s var(--ease) both; }

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

/// Available themes (the five from the Discordia design). Each sets the full
/// variable set: the legacy names components already consume, plus the design's
/// extras (`--bg2/--panel2/--up/--violet/--amber`). `--panel` and
/// `--panel-solid` share a value; the background-image path makes `--panel`
/// translucent at runtime (see `App`).
pub const THEMES: &[ThemeDef] = &[
    ThemeDef {
        id: "ember",
        label: "Ember",
        swatch: "#ff9e3d",
        vars: "--bg:#0e0b08; --bg2:#171017; --panel-solid:#17110c; --panel:#17110c; --panel2:#1e160f; \
               --edge:rgba(255,158,61,.15); --edge-strong:rgba(255,158,61,.42); \
               --border:rgba(255,158,61,.15); --border-strong:rgba(255,158,61,.42); \
               --text:#f4ece2; --text-muted:#a8988a; --text-dim:#6c5f53; \
               --accent:#ff9e3d; --accent-soft:rgba(255,158,61,.13); --accent-strong:#ffb26b; \
               --up:#5fe0a8; --success:#5fe0a8; --violet:#b98cff; --amber:#ffc46b; --warn:#ffc46b; --danger:#f2777a;",
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

/// The three bundled variable fonts (latin subset). Declared as `@font-face`
/// at runtime because the asset URLs are only known then (see `font_face_css`).
const FONT_SPACE_GROTESK: Asset = asset!("/assets/fonts/spacegrotesk.woff2");
const FONT_BRICOLAGE: Asset = asset!("/assets/fonts/bricolage.woff2");
const FONT_JETBRAINS_MONO: Asset = asset!("/assets/fonts/jetbrainsmono.woff2");

/// Build the `@font-face` block pointing at the bundled woff2 assets. They're
/// variable fonts, so one file covers the whole weight range per family.
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

/// The CSS class for a procedural background pattern id (empty for "none" or
/// when a user background image is set).
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
    let pattern = appearance.pattern.clone();
    let scrim = (appearance.background_dim.min(95) as f64) / 100.0;
    drop(appearance);
    // A user image wins over the procedural pattern.
    let pattern_class = if background.is_some() {
        ""
    } else {
        background_pattern_class(&pattern)
    };

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
        // Head elements (CSS + scripts) live in a separate prop-less component
        // so Dioxus memoizes it and never tries to diff their props — which
        // would log "Changing the props of Style/Script is not supported"
        // on every re-render of App (e.g. when moving the mic sensitivity
        // slider, which mutates the settings signal App reads).
        AppHead {}

        div {
            class: "h-screen w-screen bg-[var(--bg)] text-[var(--text)] antialiased overflow-hidden",
            style: "{root_style}",
            if !pattern_class.is_empty() {
                div { class: "{pattern_class}" }
            }
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
                    Fragment {
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
                },
            }
            }
        }
    }
}

fn session_key(p: &SessionParams) -> String {
    let mode = match &p.mode {
        SessionMode::Remote { server_url } => format!("remote:{server_url}"),
        SessionMode::SelfHost {
            allow_lan,
            rendezvous_url,
            publish_public,
            ..
        } => {
            format!(
                "selfhost:{allow_lan}:{}:{publish_public}",
                rendezvous_url.as_deref().unwrap_or("")
            )
        }
        SessionMode::ByCode {
            rendezvous_url,
            code,
        } => {
            format!("bycode:{rendezvous_url}:{code}")
        }
    };
    format!("{mode}|{}|{}", p.username, p.identity.pubkey)
}

/// All `<head>` injections: Tailwind (bundled at build time), the LiveKit SDK
/// (CDN — to be vendored in a follow-up), font faces, and the base stylesheet.
/// Extracted into a prop-less component so Dioxus memoizes it and never
/// re-evaluates it — re-rendering `App` (e.g. on settings changes) no longer
/// triggers "Changing the props of Style/Script is not supported" warnings.
#[component]
fn AppHead() -> Element {
    rsx! {
        // Tailwind utilities — inlined into the binary via `include_str!()`.
        // Renders a <style> tag in <head>. No CDN, no runtime compiler, no FOUC,
        // works offline. Same pattern as BASE_CSS and font_face_css() below.
        document::Style { {TAILWIND_CSS} }
        // LiveKit JS SDK — powers webview-side screen sharing (capture + render).
        // NB: the UMD build is `…umd.js` (there is no `.umd.min.js`); a wrong
        // path 404s silently and the lib never loads. TODO: vendor this so the
        // app works fully offline (follow-up).
        document::Script { src: "https://cdn.jsdelivr.net/npm/livekit-client@2.19.2/dist/livekit-client.umd.js" }
        document::Style { {font_face_css()} }
        document::Style { {BASE_CSS} }
    }
}
