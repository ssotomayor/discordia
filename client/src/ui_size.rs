pub fn text_css(percent: u16) -> String {
    let scale = f64::from(percent.clamp(80, 140)) / 100.0;
    let mut css = format!(".dxf-ui {{ font-size:{}px; }}", 16.0 * scale);
    for size in 8..=64 {
        css.push_str(&format!(
            ".dxf-ui [class~=\"text-[{size}px]\"] {{ font-size:{}px; }}",
            f64::from(size) * scale
        ));
    }
    for (class, size, line) in [
        ("xs", 12, 16),
        ("sm", 14, 20),
        ("base", 16, 24),
        ("lg", 18, 28),
        ("xl", 20, 28),
        ("2xl", 24, 32),
        ("3xl", 30, 36),
        ("4xl", 36, 40),
    ] {
        css.push_str(&format!(
            ".dxf-ui .text-{class} {{ font-size:{}px; line-height:{}px; }}",
            f64::from(size) * scale,
            f64::from(line) * scale
        ));
    }
    for (class, height) in [
        ("none", 1.0),
        ("tight", 1.25),
        ("snug", 1.375),
        ("normal", 1.5),
        ("relaxed", 1.625),
        ("loose", 2.0),
    ] {
        css.push_str(&format!(
            ".dxf-ui .leading-{class} {{ line-height:{height}; }}"
        ));
    }
    css
}

pub fn moved_guilds(
    visible: &[crate::protocol::Id],
    saved: &[crate::protocol::Id],
    moved: crate::protocol::Id,
    target: crate::protocol::Id,
) -> Option<Vec<crate::protocol::Id>> {
    if moved == target {
        return None;
    }
    let from = visible.iter().position(|id| *id == moved)?;
    let to = visible.iter().position(|id| *id == target)?;
    let mut next = visible.to_vec();
    next.remove(from);
    next.insert(to, moved);
    next.extend(saved.iter().filter(|id| !visible.contains(id)).copied());
    Some(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guild_moves_preserve_other_servers_and_require_visible_targets() {
        let ids: Vec<_> = (0..4).map(|_| uuid::Uuid::new_v4()).collect();
        assert_eq!(
            moved_guilds(&ids[..3], &[ids[3]], ids[0], ids[2]),
            Some(vec![ids[1], ids[2], ids[0], ids[3]])
        );
        assert_eq!(
            moved_guilds(&ids[..3], &[], ids[2], ids[0]),
            Some(vec![ids[2], ids[0], ids[1]])
        );
        assert!(moved_guilds(&ids[..3], &[], ids[0], ids[3]).is_none());
    }
    #[test]
    fn text_sizes_are_bounded_and_do_not_scale_layout_geometry() {
        assert_eq!(text_css(0), text_css(80));
        assert_eq!(text_css(u16::MAX), text_css(140));
        assert!(text_css(100).contains("font-size:14px; line-height:20px"));
        assert!(!text_css(140).contains("width:"));
    }
}
