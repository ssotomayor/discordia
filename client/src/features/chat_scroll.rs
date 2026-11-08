use crate::protocol::Id;

#[derive(serde::Deserialize)]
pub(super) struct ScrollSample {
    pub channel: Option<Id>,
    pub viewport: f64,
    #[serde(default)]
    pub pages: Vec<PageSample>,
}

#[derive(serde::Deserialize)]
pub(super) struct PageSample {
    pub id: Id,
    pub top: f64,
    pub height: f64,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct PageState {
    pub active: bool,
    pub height: f64,
}

pub(super) fn page_states(
    pages: &[PageSample],
    viewport: f64,
) -> std::collections::HashMap<Id, PageState> {
    if !viewport.is_finite() || viewport < 0.0 {
        return Default::default();
    }
    pages
        .iter()
        .filter(|p| p.top.is_finite() && p.height.is_finite() && p.height >= 0.0)
        .map(|p| {
            (
                p.id,
                PageState {
                    active: p.top <= viewport + 1200.0 && p.top + p.height >= -1200.0,
                    height: p.height,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_of_history_pages_only_mount_the_viewport_and_neighbors() {
        let pages: Vec<_> = (0..1000)
            .map(|i| PageSample {
                id: Id::new_v4(),
                top: (i as f64 - 500.0) * 4000.0,
                height: 4000.0,
            })
            .collect();
        let states = page_states(&pages, 800.0);
        assert_eq!(states.len(), 1000);
        assert_eq!(states.values().filter(|p| p.active).count(), 2);
        assert!(states[&pages[500].id].active);
        assert!(!states[&pages[0].id].active);
        assert!(page_states(&pages, f64::NAN).is_empty());
    }
}
