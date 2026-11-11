//! Where each screen share we watch is drawn (`StreamWatches`), and which
//! in-app tile, if any, has the main window to itself (`InlineView`).

use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Inline,
    /// In the external stream window (`#viewer`, trap 10).
    Detached,
}

/// A sharer absent from the map is unwatched, so a detached stream is always a watched one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamWatches(BTreeMap<String, Placement>);

impl StreamWatches {
    pub fn contains(&self, sharer: &str) -> bool {
        self.0.contains_key(sharer)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Every watched sharer, in order, wherever it is drawn.
    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.0.keys()
    }

    pub fn sharers(&self) -> HashSet<String> {
        self.0.keys().cloned().collect()
    }

    pub fn inline(&self) -> impl Iterator<Item = &String> {
        self.placed(Placement::Inline)
    }

    pub fn detached(&self) -> impl Iterator<Item = &String> {
        self.placed(Placement::Detached)
    }

    pub fn is_detached(&self, sharer: &str) -> bool {
        self.0.get(sharer) == Some(&Placement::Detached)
    }

    fn placed(&self, placement: Placement) -> impl Iterator<Item = &String> {
        self.0
            .iter()
            .filter(move |(_, at)| **at == placement)
            .map(|(sharer, _)| sharer)
    }

    /// A stream watched again comes back into the app, wherever it was before.
    pub fn watch(&mut self, sharer: impl Into<String>) {
        self.0.entry(sharer.into()).or_insert(Placement::Inline);
    }

    pub fn unwatch(&mut self, sharer: &str) -> bool {
        self.0.remove(sharer).is_some()
    }

    pub fn toggle(&mut self, sharer: &str) {
        if !self.unwatch(sharer) {
            self.watch(sharer);
        }
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.0.retain(|sharer, _| keep(sharer));
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Only a watched stream can be detached.
    pub fn detach(&mut self, sharer: &str) -> bool {
        self.place(sharer, Placement::Detached)
    }

    pub fn dock(&mut self, sharer: &str) -> bool {
        self.place(sharer, Placement::Inline)
    }

    pub fn dock_all(&mut self) -> bool {
        let mut changed = false;
        for at in self.0.values_mut() {
            changed |= *at != Placement::Inline;
            *at = Placement::Inline;
        }
        changed
    }

    fn place(&mut self, sharer: &str, placement: Placement) -> bool {
        match self.0.get_mut(sharer) {
            Some(at) if *at != placement => {
                *at = placement;
                true
            }
            _ => false,
        }
    }
}

/// The main window's streams: all of them, one focused, or one full screen.
/// Naming a tile in one value means focus and full screen cannot disagree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum InlineView {
    #[default]
    Mosaic,
    Focused(String),
    Fullscreen(String),
}

impl InlineView {
    pub fn toggle_focus(&self, sharer: &str) -> Self {
        match self {
            Self::Focused(current) if current == sharer => Self::Mosaic,
            _ => Self::Focused(sharer.to_owned()),
        }
    }

    pub fn toggle_fullscreen(&self, sharer: &str) -> Self {
        match self {
            Self::Fullscreen(current) if current == sharer => Self::Mosaic,
            _ => Self::Fullscreen(sharer.to_owned()),
        }
    }

    /// A view may name only a tile drawn in the app; focus also needs another tile to leave.
    pub fn reconciled(&self, watches: &StreamWatches) -> Self {
        let inline = |sharer: &String| watches.0.get(sharer) == Some(&Placement::Inline);
        match self {
            Self::Fullscreen(sharer) if inline(sharer) => self.clone(),
            Self::Focused(sharer) if inline(sharer) && watches.inline().count() > 1 => self.clone(),
            _ => Self::Mosaic,
        }
    }

    pub fn is_fullscreen(&self) -> bool {
        matches!(self, Self::Fullscreen(_))
    }

    /// The tile drawn alone, if any.
    pub fn selected(&self) -> Option<&String> {
        match self {
            Self::Mosaic => None,
            Self::Focused(sharer) | Self::Fullscreen(sharer) => Some(sharer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watches(inline: &[&str], detached: &[&str]) -> StreamWatches {
        let mut w = StreamWatches::default();
        for pk in inline.iter().chain(detached) {
            w.watch(*pk);
        }
        for pk in detached {
            w.detach(pk);
        }
        w
    }

    #[test]
    fn placement_moves_only_between_the_states_a_stream_can_be_in() {
        let mut w = StreamWatches::default();
        assert!(!w.detach("A"), "an unwatched stream cannot be detached");
        assert!(w.is_empty());
        w.watch("A");
        assert!(w.contains("A") && !w.is_detached("A"));
        assert!(w.detach("A"));
        assert!(!w.detach("A"));
        w.watch("A");
        assert!(
            w.is_detached("A"),
            "watching again leaves a detached stream where it is"
        );
        assert!(w.dock("A"));
        assert!(!w.dock("A"));
        w.detach("A");
        w.watch("B");
        assert_eq!(w.detached().collect::<Vec<_>>(), ["A"]);
        assert_eq!(w.inline().collect::<Vec<_>>(), ["B"]);
        assert!(w.dock_all());
        assert!(!w.dock_all());
        w.toggle("A");
        assert!(!w.contains("A"));
        w.toggle("A");
        assert!(w.contains("A") && !w.is_detached("A"));
        w.detach("A");
        w.retain(|pk| pk == "B");
        assert_eq!(w.sharers(), HashSet::from(["B".to_owned()]));
        assert!(w.detached().next().is_none());
    }

    #[test]
    fn the_inline_view_names_only_an_inline_tile() {
        use InlineView::*;
        let focused = |pk: &str| Focused(pk.into());
        let full = |pk: &str| Fullscreen(pk.into());
        let cases = [
            (focused("A"), watches(&["A", "B"], &[]), focused("A")),
            (focused("A"), watches(&["A"], &[]), Mosaic),
            (focused("A"), watches(&["A"], &["B"]), Mosaic),
            (focused("A"), watches(&["B", "C"], &["A"]), Mosaic),
            (focused("A"), watches(&["B", "C"], &[]), Mosaic),
            (full("A"), watches(&["A"], &[]), full("A")),
            (full("A"), watches(&["B"], &["A"]), Mosaic),
            (full("A"), watches(&[], &[]), Mosaic),
            (Mosaic, watches(&["A", "B"], &[]), Mosaic),
        ];
        for (view, w, expected) in cases {
            assert_eq!(view.reconciled(&w), expected, "{view:?} with {w:?}");
        }
    }

    #[test]
    fn toggles_return_to_the_mosaic() {
        use InlineView::*;
        assert_eq!(Mosaic.toggle_focus("A"), Focused("A".into()));
        assert_eq!(Focused("A".into()).toggle_focus("A"), Mosaic);
        assert_eq!(Focused("A".into()).toggle_focus("B"), Focused("B".into()));
        assert_eq!(
            Focused("A".into()).toggle_fullscreen("A"),
            Fullscreen("A".into())
        );
        assert_eq!(Fullscreen("A".into()).toggle_fullscreen("A"), Mosaic);
        assert_eq!(
            Fullscreen("A".into()).toggle_focus("A"),
            Focused("A".into())
        );
        assert_eq!(
            Fullscreen("B".into()).selected().map(String::as_str),
            Some("B")
        );
        assert!(Mosaic.selected().is_none());
    }
}
