use std::collections::HashSet;

pub(crate) fn playback_gains(
    state: &crate::state::AppState,
    previous: &[(String, f32)],
) -> Vec<(String, f32)> {
    let mut seen: Vec<String> = state.screen_shares.values().flatten().cloned().collect();
    seen.extend(state.screen_viewing.iter().cloned());
    seen.extend(previous.iter().map(|(pk, _)| pk.clone()));
    seen.sort();
    seen.dedup();
    seen.into_iter()
        .map(|pk| {
            let gain = if state.screen_viewing.contains(&pk) && !state.voice.deafened {
                state.stream_gain_of(&pk)
            } else {
                0.0
            };
            (pk, gain)
        })
        .collect()
}

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
    fn detached_stream_levels_survive_reconnect_and_stop_independently() {
        let mut state = crate::state::AppState::empty();
        state.screen_viewing.extend(["alice".into(), "bob".into()]);
        state.stream_volumes.insert("alice".into(), 25);
        state.stream_volumes.insert("bob".into(), 75);
        let initial = playback_gains(&state, &[]);
        assert_eq!(initial, vec![("alice".into(), 0.25), ("bob".into(), 0.75)]);
        state.voice_session_epoch += 1;
        assert_eq!(playback_gains(&state, &initial), initial);
        state.stream_muted.insert("alice".into());
        assert_eq!(
            playback_gains(&state, &initial),
            vec![("alice".into(), 0.0), ("bob".into(), 0.75)]
        );
        state.voice.deafened = true;
        assert!(
            playback_gains(&state, &initial)
                .iter()
                .all(|(_, gain)| *gain == 0.0)
        );
        state.voice.deafened = false;
        state.stream_muted.clear();
        assert_eq!(playback_gains(&state, &initial), initial);
        state.screen_viewing.remove("alice");
        assert_eq!(
            playback_gains(&state, &initial),
            vec![("alice".into(), 0.0), ("bob".into(), 0.75)]
        );
    }
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
