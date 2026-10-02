use std::collections::HashSet;

#[derive(Clone, Copy)]
pub(crate) enum Source {
    Voice,
    Screen,
    WebView,
}

#[derive(Clone, Default)]
pub(crate) struct Presence {
    sources: [HashSet<String>; 3],
}

pub(crate) fn identity(publisher: &str) -> &str {
    publisher
        .strip_suffix("#video")
        .or_else(|| publisher.strip_suffix("#audio"))
        .unwrap_or(publisher)
}

impl Presence {
    pub fn contains(&self, publisher: &str) -> bool {
        self.sources
            .iter()
            .any(|source| source.contains(identity(publisher)))
    }
    pub fn set(&mut self, source: Source, publisher: &str, present: bool) {
        let entries = &mut self.sources[source as usize];
        if present {
            entries.insert(identity(publisher).to_string());
        } else {
            entries.remove(identity(publisher));
        }
    }
    pub fn clear_source(&mut self, source: Source) {
        self.sources[source as usize].clear();
    }
    pub fn clear(&mut self) {
        for source in &mut self.sources {
            source.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connecting_screen_audio_or_a_webview_absence_cannot_erase_voice_stream_audio() {
        let mut presence = Presence::default();
        presence.set(Source::Voice, "alice", true);
        presence.clear_source(Source::Screen);
        presence.set(Source::WebView, "alice", false);
        assert!(presence.contains("alice"));
        presence.set(Source::Screen, "alice#video", true);
        presence.set(Source::Voice, "alice", false);
        assert!(presence.contains("alice"));
        presence.clear_source(Source::Screen);
        assert!(!presence.contains("alice"));
    }
    #[test]
    fn sources_and_participants_stop_independently_and_teardown_clears_everything() {
        let mut presence = Presence::default();
        presence.set(Source::WebView, "alice", true);
        presence.set(Source::Screen, "alice#video", true);
        presence.set(Source::Voice, "bob", true);
        presence.clear_source(Source::Screen);
        assert!(presence.contains("alice") && presence.contains("bob"));
        presence.set(Source::WebView, "alice", false);
        assert!(!presence.contains("alice") && presence.contains("bob"));
        presence.clear();
        assert!(!presence.contains("bob"));
    }
}
