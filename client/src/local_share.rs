//! Our own screen share: where it stands (`SharePhase`, changed only by
//! `ScreenShare::on`) and what the server has been told about it (`announce`).

use crate::protocol::{ClientMessage, Id};
use crate::sysvideo::{Source, Target};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SourceList {
    #[default]
    Loading,
    Listed(Vec<Source>),
    Failed(String),
}

impl SourceList {
    pub fn sources(&self) -> &[Source] {
        match self {
            Self::Listed(sources) => sources,
            Self::Loading | Self::Failed(_) => &[],
        }
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(error) => Some(error),
            Self::Loading | Self::Listed(_) => None,
        }
    }
}

/// The target lives inside the states that have one, so a share that ended
/// cannot leave its target behind for the publisher (trap 11).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SharePhase {
    #[default]
    Idle,
    /// The native source picker is open; nothing is announced yet.
    Picking(SourceList),
    /// A native source is chosen and announced; the publisher has not put it in the room yet.
    Requested {
        target: Target,
        audio: bool,
    },
    Publishing {
        target: Target,
        audio: bool,
    },
    /// The webview captured through `getDisplayMedia` and published; the page owns the source.
    WebView {
        audio: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareEvent {
    PickerOpened,
    SourcesListed(Result<Vec<Source>, String>),
    PickerClosed,
    Chosen {
        target: Target,
        audio: bool,
    },
    #[cfg_attr(
        not(any(target_os = "android", target_os = "macos", target_os = "windows")),
        allow(dead_code)
    )]
    Published(Target),
    WebViewStarted {
        native_audio: bool,
    },
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    SystemAudioLost,
    /// The person stopped it, or the page's capture ended or was never possible.
    Stopped,
    /// The native capture or its publication failed for this target.
    Failed(Target),
    CallEnded,
}

impl SharePhase {
    /// Whether we count as sharing: the server is told, and viewers may arrive.
    pub fn is_live(&self) -> bool {
        matches!(
            self,
            Self::Requested { .. } | Self::Publishing { .. } | Self::WebView { .. }
        )
    }

    /// What the native publisher should capture.
    pub fn native_target(&self) -> Option<Target> {
        match self {
            Self::Requested { target, .. } | Self::Publishing { target, .. } => Some(*target),
            Self::Idle | Self::Picking(_) | Self::WebView { .. } => None,
        }
    }

    /// Whether this computer's sound is captured natively beside the share.
    pub fn system_audio(&self) -> bool {
        match self {
            Self::Requested { audio, .. }
            | Self::Publishing { audio, .. }
            | Self::WebView { audio } => *audio,
            Self::Idle | Self::Picking(_) => false,
        }
    }

    pub fn picker(&self) -> Option<&SourceList> {
        match self {
            Self::Picking(list) => Some(list),
            _ => None,
        }
    }

    fn on(&mut self, event: ShareEvent) -> bool {
        use SharePhase::*;
        let before = self.clone();
        match event {
            ShareEvent::PickerOpened => {
                if !self.is_live() {
                    *self = Picking(SourceList::Loading);
                }
            }
            ShareEvent::SourcesListed(found) => {
                if let Picking(list) = self {
                    *list = match found {
                        Ok(sources) => SourceList::Listed(sources),
                        Err(error) => SourceList::Failed(error),
                    };
                }
            }
            ShareEvent::PickerClosed => {
                if matches!(self, Picking(_)) {
                    *self = Idle;
                }
            }
            ShareEvent::Chosen { target, audio } => {
                *self = match self {
                    Publishing { target: live, .. } if *live == target => {
                        Publishing { target, audio }
                    }
                    _ => Requested { target, audio },
                };
            }
            ShareEvent::Published(target) => {
                if let Requested {
                    target: wanted,
                    audio,
                } = *self
                    && wanted == target
                {
                    *self = Publishing { target, audio };
                }
            }
            ShareEvent::WebViewStarted { native_audio } => {
                *self = WebView {
                    audio: native_audio,
                };
            }
            ShareEvent::SystemAudioLost => match self {
                Requested { audio, .. } | Publishing { audio, .. } | WebView { audio } => {
                    *audio = false;
                }
                Idle | Picking(_) => {}
            },
            ShareEvent::Stopped => {
                if self.is_live() {
                    *self = Idle;
                }
            }
            ShareEvent::Failed(target) => {
                if self.native_target() == Some(target) {
                    *self = Idle;
                }
            }
            ShareEvent::CallEnded => *self = Idle,
        }
        *self != before
    }
}

/// The phase is private so that no caller can write one by hand.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScreenShare(SharePhase);

impl ScreenShare {
    pub fn is_live(&self) -> bool {
        self.0.is_live()
    }

    pub fn native_target(&self) -> Option<Target> {
        self.0.native_target()
    }

    pub fn system_audio(&self) -> bool {
        self.0.system_audio()
    }

    pub fn picker(&self) -> Option<&SourceList> {
        self.0.picker()
    }

    /// The only writer of the share. Returns whether anything changed.
    pub fn on(&mut self, event: ShareEvent) -> bool {
        self.0.on(event)
    }
}

/// `announced` is the channel the server was last told we share in; the result is the new one.
/// A share that follows its call into another channel is not re-announced: the move ends it.
pub fn announce(
    announced: Option<Id>,
    live: bool,
    channel: Option<Id>,
) -> (Option<Id>, Option<ClientMessage>) {
    match (live, announced, channel) {
        (true, None, Some(channel_id)) => (
            Some(channel_id),
            Some(ClientMessage::SetScreenShare {
                channel_id,
                sharing: true,
            }),
        ),
        (true, _, channel) => (channel, None),
        (false, Some(channel_id), _) => (
            None,
            Some(ClientMessage::SetScreenShare {
                channel_id,
                sharing: false,
            }),
        ),
        (false, None, _) => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Target = Target::Display(1);
    const B: Target = Target::Window(2);

    fn requested(target: Target) -> SharePhase {
        SharePhase::Requested {
            target,
            audio: true,
        }
    }

    fn publishing(target: Target) -> SharePhase {
        SharePhase::Publishing {
            target,
            audio: true,
        }
    }

    fn listed() -> SourceList {
        SourceList::Listed(vec![Source {
            target: A,
            title: "screen".into(),
            app: None,
            width: 1920,
            height: 1080,
        }])
    }

    #[test]
    fn every_transition_lands_where_the_table_says() {
        use ShareEvent as E;
        use SharePhase as S;
        let webview = S::WebView { audio: true };
        let picking = S::Picking(SourceList::Loading);
        let cases: Vec<(S, E, S)> = vec![
            (S::Idle, E::PickerOpened, picking.clone()),
            (requested(A), E::PickerOpened, requested(A)),
            (
                picking.clone(),
                E::SourcesListed(Ok(listed().sources().to_vec())),
                S::Picking(listed()),
            ),
            (
                picking.clone(),
                E::SourcesListed(Err("denied".into())),
                S::Picking(SourceList::Failed("denied".into())),
            ),
            (S::Idle, E::SourcesListed(Ok(vec![])), S::Idle),
            (picking.clone(), E::PickerClosed, S::Idle),
            (requested(A), E::PickerClosed, requested(A)),
            (
                S::Picking(listed()),
                E::Chosen {
                    target: A,
                    audio: true,
                },
                requested(A),
            ),
            (
                S::Idle,
                E::Chosen {
                    target: A,
                    audio: true,
                },
                requested(A),
            ),
            (
                publishing(A),
                E::Chosen {
                    target: B,
                    audio: true,
                },
                requested(B),
            ),
            (
                publishing(A),
                E::Chosen {
                    target: A,
                    audio: false,
                },
                S::Publishing {
                    target: A,
                    audio: false,
                },
            ),
            (requested(A), E::Published(A), publishing(A)),
            (requested(B), E::Published(A), requested(B)),
            (S::Idle, E::Published(A), S::Idle),
            (
                S::Idle,
                E::WebViewStarted { native_audio: true },
                webview.clone(),
            ),
            (
                publishing(A),
                E::SystemAudioLost,
                S::Publishing {
                    target: A,
                    audio: false,
                },
            ),
            (
                webview.clone(),
                E::SystemAudioLost,
                S::WebView { audio: false },
            ),
            (picking.clone(), E::SystemAudioLost, picking.clone()),
            (requested(A), E::Stopped, S::Idle),
            (publishing(A), E::Stopped, S::Idle),
            (webview.clone(), E::Stopped, S::Idle),
            (picking.clone(), E::Stopped, picking.clone()),
            (publishing(A), E::Failed(A), S::Idle),
            (publishing(A), E::Failed(B), publishing(A)),
            (webview.clone(), E::Failed(A), webview.clone()),
            (picking.clone(), E::CallEnded, S::Idle),
            (publishing(A), E::CallEnded, S::Idle),
        ];
        for (from, event, to) in cases {
            let mut state = from.clone();
            let changed = state.on(event.clone());
            assert_eq!(state, to, "{from:?} + {event:?}");
            assert_eq!(changed, from != to, "{from:?} + {event:?}");
        }
    }

    #[test]
    fn a_share_that_is_not_live_has_no_target_and_no_sound() {
        for state in [
            SharePhase::Idle,
            SharePhase::Picking(SourceList::Loading),
            SharePhase::Picking(listed()),
        ] {
            assert!(!state.is_live());
            assert_eq!(state.native_target(), None);
            assert!(!state.system_audio());
        }
        assert_eq!(requested(A).native_target(), Some(A));
        assert_eq!(publishing(A).native_target(), Some(A));
        assert_eq!(SharePhase::WebView { audio: true }.native_target(), None);
    }

    #[test]
    fn loading_is_not_an_empty_list() {
        assert_ne!(SourceList::Loading, SourceList::Listed(Vec::new()));
        assert!(SourceList::Loading.sources().is_empty());
        assert_eq!(SourceList::Failed("x".into()).error(), Some("x"));
    }

    #[test]
    fn the_server_hears_each_start_and_stop_once() {
        let one = Id::from_u128(1);
        let two = Id::from_u128(2);
        let on = |channel_id| {
            Some(ClientMessage::SetScreenShare {
                channel_id,
                sharing: true,
            })
        };
        let off = |channel_id| {
            Some(ClientMessage::SetScreenShare {
                channel_id,
                sharing: false,
            })
        };
        let cases = [
            ((None, true, Some(one)), (Some(one), on(one))),
            ((Some(one), true, Some(one)), (Some(one), None)),
            ((Some(one), true, Some(two)), (Some(two), None)),
            ((None, true, None), (None, None)),
            ((Some(one), false, Some(one)), (None, off(one))),
            ((Some(one), false, None), (None, off(one))),
            ((None, false, Some(one)), (None, None)),
        ];
        for ((announced, live, channel), expected) in cases {
            let got = announce(announced, live, channel);
            assert_eq!(
                (got.0, format!("{:?}", got.1)),
                (expected.0, format!("{:?}", expected.1)),
                "{announced:?} {live} {channel:?}"
            );
        }
    }
}
