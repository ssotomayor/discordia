use std::collections::HashSet;

use livekit::prelude::*;
use tokio::sync::{mpsc::UnboundedReceiver, watch};

pub(super) fn wanted(
    source: TrackSource,
    name: &str,
    publisher: &str,
    local: &str,
    screen_only: bool,
    watched: &HashSet<String>,
) -> bool {
    let publisher = publisher
        .strip_suffix("#video")
        .or_else(|| publisher.strip_suffix("#audio"))
        .unwrap_or(publisher);
    let stream = source == TrackSource::ScreenshareAudio && name != "soundboard";
    if stream {
        publisher != local && watched.contains(publisher)
    } else {
        !screen_only
    }
}

pub(super) fn refresh(room: &Room, local: &str, screen_only: bool, watched: &HashSet<String>) {
    for participant in room.remote_participants().values() {
        for publication in participant.track_publications().values() {
            let desired = wanted(
                publication.source(),
                &publication.name(),
                &participant.identity().0,
                local,
                screen_only,
                watched,
            );
            if publication.is_desired() != desired {
                publication.set_subscribed(desired);
            }
        }
    }
}

pub(super) async fn next_event(
    room: &Room,
    local: &str,
    screen_only: bool,
    watched: &mut watch::Receiver<HashSet<String>>,
    events: &mut UnboundedReceiver<RoomEvent>,
) -> Option<RoomEvent> {
    loop {
        tokio::select! {
            changed = watched.changed() => {
                if changed.is_err() { return None; }
                refresh(room, local, screen_only, &watched.borrow_and_update());
            }
            event = events.recv() => {
                if matches!(event, Some(RoomEvent::TrackPublished { .. } | RoomEvent::Reconnected)) {
                    refresh(room, local, screen_only, &watched.borrow());
                }
                return event;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_controls_only_stream_audio_and_excludes_self() {
        let mut watched = HashSet::new();
        for screen_only in [false, true] {
            assert!(!wanted(
                TrackSource::ScreenshareAudio,
                "screen",
                "alice#video",
                "me",
                screen_only,
                &watched
            ));
            watched.insert("alice".into());
            assert!(wanted(
                TrackSource::ScreenshareAudio,
                "screen",
                "alice#video",
                "me",
                screen_only,
                &watched
            ));
            assert!(!wanted(
                TrackSource::ScreenshareAudio,
                "screen",
                "alice#audio",
                "alice",
                screen_only,
                &watched
            ));
            watched.clear();
        }
        assert!(wanted(
            TrackSource::Microphone,
            "mic",
            "alice",
            "me",
            false,
            &watched
        ));
        assert!(wanted(
            TrackSource::ScreenshareAudio,
            "soundboard",
            "alice",
            "me",
            false,
            &watched
        ));
        assert!(!wanted(
            TrackSource::Microphone,
            "mic",
            "alice",
            "me",
            true,
            &watched
        ));
    }
}
