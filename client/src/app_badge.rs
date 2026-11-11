use dioxus::prelude::*;

use crate::state::{AppBadge, AppState};

/// Whether we are in front is what decides if an open conversation is being
/// read, and so whether anything reaches the badge at all.
pub(crate) fn use_foreground(mut state: Signal<AppState>) {
    crate::platform::use_wry_event_handler(move |event, _| {
        use crate::platform::tao::event::{Event, WindowEvent};
        // Any window counts: replying from the Social window is reading.
        if let Event::WindowEvent {
            event: WindowEvent::Focused(focused),
            ..
        } = event
            && state.peek().app_foreground != *focused
        {
            state.write().app_foreground = *focused;
        }
    });
    let unread_on_screen = use_memo(move || state.read().unread_on_screen());
    use_effect(move || {
        if let Some(channel) = unread_on_screen() {
            state.write().mark_dm_read(channel);
        }
    });
}

pub(crate) fn use_badge(state: Signal<AppState>) {
    let window = crate::platform::use_window();
    // Focus is in here only to re-apply: Windows forgets an overlay set while
    // the taskbar button is missing, as it is in the tray or before first show.
    let badge = use_memo(move || {
        let s = state.read();
        (s.app_badge(), s.app_foreground)
    });
    let shown = window.clone();
    use_effect(move || show(&shown, badge().0));
    use_drop(move || show(&window, AppBadge::None));
}

#[cfg(target_os = "macos")]
fn show(window: &crate::platform::DesktopContext, badge: AppBadge) {
    use crate::platform::tao::platform::macos::WindowExtMacOS;
    window.window.set_badge_label(match badge {
        AppBadge::None => None,
        AppBadge::Unread => Some("•".into()),
        AppBadge::Count(n) => Some(n.to_string()),
    });
}

#[cfg(target_os = "windows")]
fn show(window: &crate::platform::DesktopContext, badge: AppBadge) {
    use crate::platform::tao::platform::windows::WindowExtWindows;
    use crate::platform::tao::window::Icon;
    let icon = overlay_rgba(badge).and_then(|rgba| Icon::from_rgba(rgba, SIZE, SIZE).ok());
    window.window.set_overlay_icon(icon.as_ref());
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn show(_window: &crate::platform::DesktopContext, _badge: AppBadge) {}

#[cfg(any(target_os = "windows", test))]
const SIZE: u32 = 32;

#[cfg(any(target_os = "windows", test))]
const RED: [u8; 3] = [0xe5, 0x3e, 0x3e];

/// 3×5 digits and a plus, one row per `u8`, high bit on the left. Windows draws
/// the overlay at 16 px on a normal display, too small for any real typeface.
#[cfg(any(target_os = "windows", test))]
fn glyph(c: char) -> [u8; 5] {
    match c {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b001, 0b001, 0b001],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        _ => [0b000, 0b010, 0b111, 0b010, 0b000],
    }
}

/// A red disc, with the count in white when there is one. Past 9 it reads
/// "9+": two digits at this size are a smudge.
#[cfg(any(target_os = "windows", test))]
fn overlay_rgba(badge: AppBadge) -> Option<Vec<u8>> {
    let (radius, text) = match badge {
        AppBadge::None => return None,
        AppBadge::Unread => (10.0, String::new()),
        AppBadge::Count(n) if n > 9 => (16.0, "9+".to_string()),
        AppBadge::Count(n) => (16.0, n.to_string()),
    };
    let scale = if text.chars().count() > 1 { 3 } else { 4 };
    let glyphs: Vec<[u8; 5]> = text.chars().map(glyph).collect();
    let width = (glyphs.len() as u32 * 4).saturating_sub(1) * scale;
    let left = SIZE.saturating_sub(width) / 2;
    let top = (SIZE - 5 * scale) / 2;
    let ink = |x: u32, y: u32| {
        if x < left || y < top {
            return false;
        }
        let (gx, gy) = ((x - left) / scale, (y - top) / scale);
        let (index, column) = ((gx / 4) as usize, gx % 4);
        gy < 5
            && column < 3
            && glyphs
                .get(index)
                .is_some_and(|g| (g[gy as usize] >> (2 - column)) & 1 == 1)
    };
    let centre = SIZE as f32 / 2.0;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = ((x as f32 + 0.5 - centre).powi(2) + (y as f32 + 0.5 - centre).powi(2)).sqrt();
            let alpha = (radius - d + 0.5).clamp(0.0, 1.0);
            let [r, g, b] = if ink(x, y) { [0xff; 3] } else { RED };
            rgba.extend_from_slice(&[r, g, b, (alpha * 255.0).round() as u8]);
        }
    }
    Some(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(rgba: &[u8], x: u32, y: u32) -> [u8; 4] {
        let i = ((y * SIZE + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    #[test]
    fn nothing_unread_clears_the_overlay() {
        assert!(overlay_rgba(AppBadge::None).is_none());
    }

    #[test]
    fn a_plain_unread_is_a_small_red_dot() {
        let rgba = overlay_rgba(AppBadge::Unread).unwrap();
        assert_eq!(rgba.len(), (SIZE * SIZE * 4) as usize);
        assert_eq!(pixel(&rgba, 16, 16), [RED[0], RED[1], RED[2], 255]);
        assert_eq!(pixel(&rgba, 1, 16)[3], 0);
    }

    #[test]
    fn a_count_draws_white_ink_on_the_disc() {
        let one = overlay_rgba(AppBadge::Count(1)).unwrap();
        // The stem of the "1" is the glyph's middle column.
        assert_eq!(pixel(&one, 16, 16), [0xff, 0xff, 0xff, 255]);
        assert_eq!(pixel(&one, 3, 16), [RED[0], RED[1], RED[2], 255]);
        assert_ne!(
            overlay_rgba(AppBadge::Count(12)),
            overlay_rgba(AppBadge::Count(2))
        );
        assert_eq!(
            overlay_rgba(AppBadge::Count(12)),
            overlay_rgba(AppBadge::Count(40))
        );
    }
}
