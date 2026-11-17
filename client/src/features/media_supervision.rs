use livekit::RoomEvent;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) async fn observe_publisher(
    mut events: tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
    reason: Arc<Mutex<Option<String>>>,
) {
    let mut ice_hint = None;
    while let Some(event) = events.recv().await {
        match event {
            RoomEvent::Disconnected { reason: ended } => {
                *reason.lock() = Some(format!(
                    "{ended:?}; {}",
                    ice_hint
                        .as_deref()
                        .unwrap_or("no ICE server error reported")
                ));
                break;
            }
            RoomEvent::IceServerError {
                publisher,
                server_type,
                error_code,
            } => {
                tracing::warn!(diagnostic_code = "ICE-SERVER", publisher, %server_type, error_code, "video ICE server request failed; other candidates may still connect");
                ice_hint = Some(format!("ICE-{server_type} {error_code}"));
            }
            RoomEvent::Reconnecting => tracing::info!("native publisher reconnecting"),
            RoomEvent::Reconnected => {
                ice_hint = None;
                tracing::info!("native publisher reconnected");
            }
            _ => {}
        }
    }
}

pub(super) struct StatsHealth {
    last_valid: Instant,
}

impl StatsHealth {
    pub fn new(now: Instant) -> Self {
        Self { last_valid: now }
    }

    pub fn reset(&mut self, now: Instant) {
        self.last_valid = now;
    }

    pub fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_valid) >= Duration::from_secs(15)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consecutive_errors_expire_but_a_valid_report_or_reconnect_resets_the_budget() {
        let start = Instant::now();
        let mut health = StatsHealth::new(start);
        assert!(!health.expired(start + Duration::from_secs(14)));
        assert!(health.expired(start + Duration::from_secs(15)));
        health.reset(start + Duration::from_secs(14));
        assert!(!health.expired(start + Duration::from_secs(28)));
        assert!(health.expired(start + Duration::from_secs(29)));
        health.reset(start + Duration::from_secs(40));
        assert!(!health.expired(start + Duration::from_secs(40)));
    }

    #[tokio::test]
    async fn reconnecting_is_not_terminal_and_recovery_clears_old_ice_failures() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let reason = Arc::new(Mutex::new(None));
        let task = tokio::spawn(observe_publisher(rx, reason.clone()));
        tx.send(RoomEvent::IceServerError {
            publisher: true,
            server_type: "turn".into(),
            error_code: 701,
        })
        .unwrap();
        tx.send(RoomEvent::Reconnecting).unwrap();
        tokio::task::yield_now().await;
        assert!(reason.lock().is_none());
        tx.send(RoomEvent::Reconnected).unwrap();
        tx.send(RoomEvent::Disconnected {
            reason: livekit::DisconnectReason::ClientInitiated,
        })
        .unwrap();
        task.await.unwrap();
        let ended = reason.lock().clone().unwrap();
        assert!(ended.contains("ClientInitiated"));
        assert!(!ended.contains("ICE-turn"));
    }
}
