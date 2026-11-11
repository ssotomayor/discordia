//! Our own camera: where it stands (`CameraPhase`, changed only by
//! `LocalCamera::on`) and what the server must hear after each change.

use crate::protocol::ClientMessage;

/// One value, so a camera cannot be on and still starting, or set up while live.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CameraPhase {
    #[default]
    Off,
    /// The setup dialog is previewing; nothing is published.
    Setup,
    /// Asked for and not yet in the room. A cancelled start goes back to `Off`.
    Starting,
    /// Published. A webview room that reconnects republishes from here.
    On,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraEvent {
    SetupOpened,
    SetupClosed,
    StartRequested,
    /// The publisher put the camera in the room.
    Started,
    /// The person turned it off. The server hears it here (trap 29).
    Stopped,
    /// The webview reported the track ended; a native publisher never sends it.
    #[cfg_attr(any(target_os = "android", target_os = "windows"), allow(dead_code))]
    Ended,
    Failed,
    /// The call or its video grant is gone; the server already knows.
    Released,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalCamera(CameraPhase);

impl LocalCamera {
    #[cfg(test)]
    pub fn phase(&self) -> CameraPhase {
        self.0
    }

    pub fn is_on(&self) -> bool {
        self.0 == CameraPhase::On
    }

    pub fn is_starting(&self) -> bool {
        self.0 == CameraPhase::Starting
    }

    /// Whether a publisher should hold the camera in the room.
    pub fn wanted(&self) -> bool {
        matches!(self.0, CameraPhase::Starting | CameraPhase::On)
    }

    pub fn setup_open(&self) -> bool {
        self.0 == CameraPhase::Setup
    }

    /// The only writer of the camera's phase.
    pub fn on(&mut self, event: CameraEvent) -> CameraFollowup {
        use CameraPhase::*;
        let before = self.0;
        let mut server = None;
        self.0 = match (event, before) {
            (CameraEvent::SetupOpened, Off) => Setup,
            (CameraEvent::SetupClosed, Setup) => Off,
            (CameraEvent::StartRequested, Off | Setup | Starting) => Starting,
            (CameraEvent::Started, _) => {
                // Sent again on a republish: the server may have dropped it meanwhile.
                server = Some(true);
                On
            }
            (CameraEvent::Stopped, Starting | On) => {
                server = Some(false);
                Off
            }
            (CameraEvent::Ended | CameraEvent::Failed, Starting | On) => {
                server = (before == On).then_some(false);
                Off
            }
            (CameraEvent::Released, _) => Off,
            (_, phase) => phase,
        };
        CameraFollowup {
            changed: self.0 != before,
            server,
        }
    }
}

/// What has to follow a camera change, so no caller decides on its own whether the server hears.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CameraFollowup {
    pub changed: bool,
    server: Option<bool>,
}

impl CameraFollowup {
    pub fn server(&self) -> Option<ClientMessage> {
        self.server.map(|on| ClientMessage::SetCamera { on })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_transition_lands_where_the_table_says() {
        use CameraEvent as E;
        use CameraPhase::*;
        let all = [Off, Setup, Starting, On];
        // (event, from, to, server)
        let cases: &[(E, CameraPhase, CameraPhase, Option<bool>)] = &[
            (E::SetupOpened, Off, Setup, None),
            (E::SetupOpened, Starting, Starting, None),
            (E::SetupOpened, On, On, None),
            (E::SetupClosed, Setup, Off, None),
            (E::SetupClosed, On, On, None),
            (E::StartRequested, Off, Starting, None),
            (E::StartRequested, Setup, Starting, None),
            (E::StartRequested, Starting, Starting, None),
            (E::StartRequested, On, On, None),
            (E::Started, Starting, On, Some(true)),
            (E::Started, On, On, Some(true)),
            (E::Started, Off, On, Some(true)),
            (E::Stopped, Starting, Off, Some(false)),
            (E::Stopped, On, Off, Some(false)),
            (E::Stopped, Off, Off, None),
            (E::Stopped, Setup, Setup, None),
            (E::Ended, On, Off, Some(false)),
            (E::Ended, Starting, Off, None),
            (E::Ended, Setup, Setup, None),
            (E::Failed, On, Off, Some(false)),
            (E::Failed, Starting, Off, None),
            (E::Failed, Off, Off, None),
            (E::Released, On, Off, None),
            (E::Released, Setup, Off, None),
            (E::Released, Starting, Off, None),
        ];
        for &(event, from, to, server) in cases {
            let mut camera = LocalCamera(from);
            let followup = camera.on(event);
            assert_eq!(camera.phase(), to, "{from:?} + {event:?}");
            assert_eq!(followup.changed, from != to, "{from:?} + {event:?}");
            assert_eq!(
                format!("{:?}", followup.server()),
                format!("{:?}", server.map(|on| ClientMessage::SetCamera { on })),
                "{from:?} + {event:?}"
            );
        }
        for from in all {
            let camera = LocalCamera(from);
            assert!(!(camera.is_on() && camera.is_starting()));
            assert_eq!(camera.wanted(), camera.is_on() || camera.is_starting());
        }
    }
}
