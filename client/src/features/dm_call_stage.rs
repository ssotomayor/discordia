use std::time::{Duration, Instant};

use dioxus::core::Task;

use super::Phase;
use crate::nostr::call_history::Outcome;
use crate::nostr::calls::{Body, EndReason};

const RING: Duration = Duration::from_secs(45);
const CONNECT: Duration = Duration::from_secs(60);
const RECONNECT: Duration = Duration::from_secs(15);

/// Where a DM call is. Each variant holds only what exists in it: the
/// deadline in the states that time out, the running job in the states that wait on one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stage {
    Incoming {
        deadline: Instant,
    },
    /// Leaving guild voice before answering; `None` once it has left.
    Switching {
        deadline: Instant,
        handoff: Option<Task>,
    },
    Ringing {
        deadline: Instant,
    },
    Connecting {
        deadline: Instant,
        step: Negotiation,
    },
    Connected,
    Reconnecting {
        deadline: Instant,
    },
}

/// Inside `Connecting`: who owes which description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Negotiation {
    /// The callee accepted and waits for the caller's offer.
    AwaitingOffer,
    /// Our offer or answer is being made and its candidates gathered.
    Gathering(Task),
    /// The caller sent its offer.
    AwaitingAnswer,
    ApplyingAnswer(Task),
    /// Both descriptions are set; ICE decides the rest.
    Established,
}

impl Stage {
    pub(super) fn incoming(now: Instant) -> Self {
        Self::Incoming {
            deadline: now + RING,
        }
    }

    pub(super) fn ringing(now: Instant) -> Self {
        Self::Ringing {
            deadline: now + RING,
        }
    }

    pub(super) fn phase(self) -> Phase {
        match self {
            Self::Incoming { .. } => Phase::Incoming,
            Self::Switching { .. } => Phase::Switching,
            Self::Ringing { .. } => Phase::Ringing,
            Self::Connecting { .. } => Phase::Connecting,
            Self::Connected => Phase::Connected,
            Self::Reconnecting { .. } => Phase::Reconnecting,
        }
    }

    pub(super) fn deadline(self) -> Option<Instant> {
        match self {
            Self::Incoming { deadline }
            | Self::Switching { deadline, .. }
            | Self::Ringing { deadline }
            | Self::Connecting { deadline, .. }
            | Self::Reconnecting { deadline } => Some(deadline),
            Self::Connected => None,
        }
    }

    pub(super) fn job(self) -> Option<Task> {
        match self {
            Self::Switching {
                handoff: Some(job), ..
            }
            | Self::Connecting {
                step: Negotiation::Gathering(job) | Negotiation::ApplyingAnswer(job),
                ..
            } => Some(job),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Event {
    /// The person pressed Accept.
    Press {
        guild_busy: bool,
    },
    /// Accept, once the voice service is ready and any guild call is gone.
    Accept {
        guild_busy: bool,
    },
    Hangup,
    /// Only Android's voice service can fail outside the call's own stages.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    Fail(String),
    Tick {
        guild_busy: bool,
    },
    HandoffDone {
        result: Result<(), String>,
        guild_busy: bool,
    },
    /// The call's signal reached no relay.
    Unpublished,
    Accepted {
        device: uuid::Uuid,
    },
    Offer(String),
    Answer(String),
    Ended(EndReason),
    /// The caller sent its offer to another of our devices.
    AnsweredElsewhere,
    Connected,
    Disconnected,
    /// The peer connection failed or closed.
    Lost,
    Described(Result<(bool, String), String>),
    AnswerApplied(Result<(), String>),
}

/// Work a transition needs started; its task becomes part of the next stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Job {
    Handoff,
    /// Make our description: the answer to this offer, or an offer.
    Describe(Option<String>),
    ApplyAnswer(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Effect {
    Record(Outcome),
    Send(Body),
    /// The device that answered becomes the only one we talk to.
    Bind(uuid::Uuid),
    /// Go on to `Event::Accept` once the voice service is ready.
    Answer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Ending {
    pub signal: Option<EndReason>,
    pub record: Option<Outcome>,
    pub toast: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Next {
    Stay,
    To(Stage),
    End(Ending),
}

/// Effects run before `next` is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Transition {
    pub effects: Vec<Effect>,
    pub next: Next,
}

impl Transition {
    fn to(stage: Stage, effects: Vec<Effect>) -> Self {
        Self {
            effects,
            next: Next::To(stage),
        }
    }

    fn end(ending: Ending) -> Self {
        Self {
            effects: Vec::new(),
            next: Next::End(ending),
        }
    }
}

/// We end the call and tell the peer why.
fn hang_up(phase: Phase, reason: EndReason, toast: Option<&str>) -> Ending {
    let record = match (phase, reason) {
        (_, EndReason::Failed) => Some(Outcome::Failed),
        (Phase::Ringing, EndReason::Timeout | EndReason::Hangup) => Some(Outcome::Missed),
        (Phase::Incoming, EndReason::Declined) => Some(Outcome::Declined),
        (Phase::Incoming | Phase::Switching, EndReason::Timeout) => Some(Outcome::Missed),
        _ => None,
    };
    Ending {
        signal: Some(reason),
        record,
        toast: toast.map(str::to_owned),
    }
}

/// The peer ended the call.
fn hung_up(phase: Phase, reason: EndReason) -> Ending {
    let record = match reason {
        EndReason::Failed => Some(Outcome::Failed),
        EndReason::Busy => Some(Outcome::Busy),
        EndReason::Declined => Some(Outcome::Declined),
        EndReason::Hangup | EndReason::Timeout
            if matches!(phase, Phase::Incoming | Phase::Switching) =>
        {
            Some(Outcome::Missed)
        }
        _ => None,
    };
    let toast = match reason {
        EndReason::Busy => Some("This contact is busy."),
        EndReason::Declined => Some("The call was declined."),
        EndReason::Failed => Some("The contact could not connect."),
        EndReason::Hangup | EndReason::Timeout => None,
    };
    Ending {
        signal: None,
        record,
        toast: toast.map(str::to_owned),
    }
}

fn failed(phase: Phase, error: &str) -> Transition {
    Transition::end(hang_up(phase, EndReason::Failed, Some(error)))
}

/// The one place a DM call changes stage. `None` refuses an event the stage
/// does not take; `start` begins the job a stage waits on.
pub(super) fn transition(
    stage: Stage,
    event: Event,
    now: Instant,
    start: &mut dyn FnMut(Job) -> Result<Task, String>,
) -> Option<Transition> {
    use Negotiation::*;
    use Stage::*;
    let phase = stage.phase();
    let expired = stage.deadline().is_some_and(|deadline| now >= deadline);
    Some(match (stage, event) {
        (Incoming { deadline }, Event::Press { guild_busy: true }) => match start(Job::Handoff) {
            Ok(job) => Transition::to(
                Switching {
                    deadline,
                    handoff: Some(job),
                },
                Vec::new(),
            ),
            Err(error) => failed(phase, &error),
        },
        (Incoming { .. }, Event::Press { guild_busy: false }) => Transition {
            effects: vec![Effect::Answer],
            next: Next::Stay,
        },
        (Incoming { .. } | Switching { .. }, Event::Accept { guild_busy: true }) => {
            Transition::end(hang_up(phase, EndReason::Busy, None))
        }
        (Incoming { .. } | Switching { .. }, Event::Accept { guild_busy: false }) => {
            Transition::to(
                Connecting {
                    deadline: now + CONNECT,
                    step: AwaitingOffer,
                },
                vec![
                    Effect::Record(Outcome::Answered),
                    Effect::Send(Body::Accept),
                ],
            )
        }
        (
            Switching {
                deadline,
                handoff: Some(_),
            },
            Event::HandoffDone { result, guild_busy },
        ) => {
            if expired {
                Transition::end(hang_up(phase, EndReason::Timeout, None))
            } else if let Err(error) = result {
                failed(phase, &error)
            } else if guild_busy {
                failed(phase, "Could not leave the voice channel.")
            } else {
                Transition::to(
                    Switching {
                        deadline,
                        handoff: None,
                    },
                    vec![Effect::Answer],
                )
            }
        }
        (_, Event::Hangup) => Transition::end(hang_up(
            phase,
            if phase == Phase::Incoming {
                EndReason::Declined
            } else {
                EndReason::Hangup
            },
            None,
        )),
        (_, Event::Fail(error)) => failed(phase, &error),
        (_, Event::Unpublished) => failed(phase, "The call signal could not reach a relay."),
        (_, Event::Tick { .. }) if expired => Transition::end(hang_up(
            phase,
            EndReason::Timeout,
            Some("The call timed out. The contact may be offline or unreachable."),
        )),
        (_, Event::Tick { guild_busy: true })
            if !matches!(phase, Phase::Incoming | Phase::Switching) =>
        {
            Transition::end(hang_up(
                phase,
                EndReason::Busy,
                Some("DM call ended because a voice channel was joined."),
            ))
        }
        (Ringing { .. }, Event::Accepted { device }) => {
            let effects = vec![Effect::Bind(device), Effect::Record(Outcome::Answered)];
            match start(Job::Describe(None)) {
                Ok(job) => Transition::to(
                    Connecting {
                        deadline: now + CONNECT,
                        step: Gathering(job),
                    },
                    effects,
                ),
                Err(error) => Transition {
                    effects,
                    next: Next::End(hang_up(Phase::Connecting, EndReason::Failed, Some(&error))),
                },
            }
        }
        (
            Connecting {
                deadline,
                step: AwaitingOffer,
            },
            Event::Offer(sdp),
        ) => match start(Job::Describe(Some(sdp))) {
            Ok(job) => Transition::to(
                Connecting {
                    deadline,
                    step: Gathering(job),
                },
                Vec::new(),
            ),
            Err(error) => failed(phase, &error),
        },
        (
            Connecting {
                deadline,
                step: Gathering(_),
            },
            Event::Described(result),
        ) => match result {
            Ok((true, sdp)) => Transition::to(
                Connecting {
                    deadline,
                    step: Established,
                },
                vec![Effect::Send(Body::Answer { sdp })],
            ),
            Ok((false, sdp)) => Transition::to(
                Connecting {
                    deadline,
                    step: AwaitingAnswer,
                },
                vec![Effect::Send(Body::Offer { sdp })],
            ),
            Err(error) => failed(phase, &error),
        },
        (
            Connecting {
                deadline,
                step: AwaitingAnswer,
            },
            Event::Answer(sdp),
        ) => match start(Job::ApplyAnswer(sdp)) {
            Ok(job) => Transition::to(
                Connecting {
                    deadline,
                    step: ApplyingAnswer(job),
                },
                Vec::new(),
            ),
            Err(error) => failed(phase, &error),
        },
        (
            Connecting {
                deadline,
                step: ApplyingAnswer(_),
            },
            Event::AnswerApplied(result),
        ) => match result {
            Ok(()) => Transition::to(
                Connecting {
                    deadline,
                    step: Established,
                },
                Vec::new(),
            ),
            Err(error) => failed(phase, &error),
        },
        (_, Event::Ended(reason)) => Transition::end(hung_up(phase, reason)),
        (Incoming { .. } | Switching { .. } | Connecting { .. }, Event::AnsweredElsewhere) => {
            Transition::end(Ending {
                signal: None,
                record: None,
                toast: None,
            })
        }
        (Connecting { .. } | Reconnecting { .. }, Event::Connected) => {
            Transition::to(Connected, Vec::new())
        }
        (Connected, Event::Disconnected) => Transition::to(
            Reconnecting {
                deadline: now + RECONNECT,
            },
            Vec::new(),
        ),
        (_, Event::Lost) => failed(
            phase,
            "Voice connection ended. Check your network and TURN configuration.",
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::{Effect, Event};
    use crate::render_check::Checked;
    use dioxus::prelude::*;
    use uuid::Uuid;

    fn in_scope(test: impl FnOnce()) {
        let mut dom = VirtualDom::new(|| rsx! {});
        dom.rebuild_checked();
        dom.in_scope(ScopeId::ROOT, test);
    }

    fn ending(signal: Option<EndReason>, record: Option<Outcome>, toast: Option<&str>) -> Next {
        Next::End(Ending {
            signal,
            record,
            toast: toast.map(str::to_owned),
        })
    }

    /// Runs one transition with a starter that hands out `job` and records what it was asked for.
    fn run(stage: Stage, event: Event, now: Instant, job: Task) -> (Option<Transition>, Vec<Job>) {
        let mut asked = Vec::new();
        let result = transition(stage, event, now, &mut |wanted| {
            asked.push(wanted);
            Ok(job)
        });
        (result, asked)
    }

    /// From, event, the effects and next stage (or a refusal), the job asked for.
    type Row = (Stage, Event, Option<(Vec<Effect>, Next)>, Option<Job>);

    #[test]
    fn every_dm_call_transition() {
        in_scope(|| {
            use Negotiation::*;
            use Stage::*;
            let now = Instant::now();
            let later = now + Duration::from_secs(5);
            let old = spawn(async {});
            let job = spawn(async {});
            let device = Uuid::new_v4();
            let sdp = || "v=0".to_string();
            let connecting = |step| Connecting {
                deadline: later,
                step,
            };
            let to = |stage, effects: Vec<Effect>| Some((effects, Next::To(stage)));
            let end = |next| Some((Vec::new(), next));
            let timeout = "The call timed out. The contact may be offline or unreachable.";
            let joined = "DM call ended because a voice channel was joined.";
            let lost = "Voice connection ended. Check your network and TURN configuration.";
            #[rustfmt::skip]
            let table: Vec<Row> = vec![
                (Incoming { deadline: later }, Event::Press { guild_busy: true },
                    to(Switching { deadline: later, handoff: Some(job) }, vec![]), Some(Job::Handoff)),
                (Incoming { deadline: later }, Event::Press { guild_busy: false },
                    Some((vec![Effect::Answer], Next::Stay)), None),
                (Switching { deadline: later, handoff: Some(old) }, Event::Press { guild_busy: true }, None, None),
                (Switching { deadline: later, handoff: None }, Event::Press { guild_busy: false }, None, None),
                (Ringing { deadline: later }, Event::Press { guild_busy: false }, None, None),
                (Incoming { deadline: later }, Event::Accept { guild_busy: false },
                    to(connecting(AwaitingOffer).with_deadline(now + CONNECT), vec![Effect::Record(Outcome::Answered), Effect::Send(Body::Accept)]), None),
                (Switching { deadline: later, handoff: None }, Event::Accept { guild_busy: false },
                    to(connecting(AwaitingOffer).with_deadline(now + CONNECT), vec![Effect::Record(Outcome::Answered), Effect::Send(Body::Accept)]), None),
                (Incoming { deadline: later }, Event::Accept { guild_busy: true },
                    end(ending(Some(EndReason::Busy), None, None)), None),
                (Connected, Event::Accept { guild_busy: true }, None, None),
                (Ringing { deadline: later }, Event::Accept { guild_busy: false }, None, None),
                (Switching { deadline: later, handoff: Some(old) }, Event::HandoffDone { result: Ok(()), guild_busy: false },
                    to(Switching { deadline: later, handoff: None }, vec![Effect::Answer]), None),
                (Switching { deadline: now, handoff: Some(old) }, Event::HandoffDone { result: Ok(()), guild_busy: false },
                    end(ending(Some(EndReason::Timeout), Some(Outcome::Missed), None)), None),
                (Switching { deadline: later, handoff: Some(old) }, Event::HandoffDone { result: Err("stuck".into()), guild_busy: false },
                    end(ending(Some(EndReason::Failed), Some(Outcome::Failed), Some("stuck"))), None),
                (Switching { deadline: later, handoff: Some(old) }, Event::HandoffDone { result: Ok(()), guild_busy: true },
                    end(ending(Some(EndReason::Failed), Some(Outcome::Failed), Some("Could not leave the voice channel."))), None),
                (Switching { deadline: later, handoff: None }, Event::HandoffDone { result: Ok(()), guild_busy: false }, None, None),
                (Incoming { deadline: later }, Event::HandoffDone { result: Ok(()), guild_busy: false }, None, None),
                (Incoming { deadline: later }, Event::Hangup, end(ending(Some(EndReason::Declined), Some(Outcome::Declined), None)), None),
                (Ringing { deadline: later }, Event::Hangup, end(ending(Some(EndReason::Hangup), Some(Outcome::Missed), None)), None),
                (Switching { deadline: later, handoff: Some(old) }, Event::Hangup, end(ending(Some(EndReason::Hangup), None, None)), None),
                (Connected, Event::Hangup, end(ending(Some(EndReason::Hangup), None, None)), None),
                (Ringing { deadline: later }, Event::Fail("x".into()), end(ending(Some(EndReason::Failed), Some(Outcome::Failed), Some("x"))), None),
                (Connected, Event::Unpublished,
                    end(ending(Some(EndReason::Failed), Some(Outcome::Failed), Some("The call signal could not reach a relay."))), None),
                (Ringing { deadline: now }, Event::Tick { guild_busy: false },
                    end(ending(Some(EndReason::Timeout), Some(Outcome::Missed), Some(timeout))), None),
                (Incoming { deadline: now }, Event::Tick { guild_busy: true },
                    end(ending(Some(EndReason::Timeout), Some(Outcome::Missed), Some(timeout))), None),
                (Reconnecting { deadline: now }, Event::Tick { guild_busy: false },
                    end(ending(Some(EndReason::Timeout), None, Some(timeout))), None),
                (Connected, Event::Tick { guild_busy: false }, None, None),
                (Connected, Event::Tick { guild_busy: true }, end(ending(Some(EndReason::Busy), None, Some(joined))), None),
                (Ringing { deadline: later }, Event::Tick { guild_busy: true }, end(ending(Some(EndReason::Busy), None, Some(joined))), None),
                (Incoming { deadline: later }, Event::Tick { guild_busy: true }, None, None),
                (Switching { deadline: later, handoff: Some(old) }, Event::Tick { guild_busy: true }, None, None),
                (Ringing { deadline: later }, Event::Accepted { device },
                    Some((vec![Effect::Bind(device), Effect::Record(Outcome::Answered)], Next::To(connecting(Gathering(job)).with_deadline(now + CONNECT)))),
                    Some(Job::Describe(None))),
                (Incoming { deadline: later }, Event::Accepted { device }, None, None),
                (connecting(AwaitingAnswer), Event::Accepted { device }, None, None),
                (connecting(AwaitingOffer), Event::Offer(sdp()), to(connecting(Gathering(job)), vec![]), Some(Job::Describe(Some(sdp())))),
                (connecting(Gathering(old)), Event::Offer(sdp()), None, None),
                (connecting(Established), Event::Offer(sdp()), None, None),
                (Incoming { deadline: later }, Event::Offer(sdp()), None, None),
                (connecting(Gathering(old)), Event::Described(Ok((true, sdp()))),
                    to(connecting(Established), vec![Effect::Send(Body::Answer { sdp: sdp() })]), None),
                (connecting(Gathering(old)), Event::Described(Ok((false, sdp()))),
                    to(connecting(AwaitingAnswer), vec![Effect::Send(Body::Offer { sdp: sdp() })]), None),
                (connecting(Gathering(old)), Event::Described(Err("no route".into())),
                    end(ending(Some(EndReason::Failed), Some(Outcome::Failed), Some("no route"))), None),
                (connecting(AwaitingAnswer), Event::Described(Ok((false, sdp()))), None, None),
                (connecting(AwaitingAnswer), Event::Answer(sdp()), to(connecting(ApplyingAnswer(job)), vec![]), Some(Job::ApplyAnswer(sdp()))),
                (connecting(ApplyingAnswer(old)), Event::Answer(sdp()), None, None),
                // The callee has sent its answer: an answer back is the peer's mistake.
                (connecting(Established), Event::Answer(sdp()), None, None),
                (connecting(AwaitingOffer), Event::Answer(sdp()), None, None),
                (Connected, Event::Answer(sdp()), None, None),
                (connecting(ApplyingAnswer(old)), Event::AnswerApplied(Ok(())), to(connecting(Established), vec![]), None),
                (connecting(ApplyingAnswer(old)), Event::AnswerApplied(Err("bad".into())),
                    end(ending(Some(EndReason::Failed), Some(Outcome::Failed), Some("bad"))), None),
                (Connected, Event::AnswerApplied(Ok(())), None, None),
                (Incoming { deadline: later }, Event::Ended(EndReason::Hangup), end(ending(None, Some(Outcome::Missed), None)), None),
                (Switching { deadline: later, handoff: Some(old) }, Event::Ended(EndReason::Timeout), end(ending(None, Some(Outcome::Missed), None)), None),
                (Ringing { deadline: later }, Event::Ended(EndReason::Hangup), end(ending(None, None, None)), None),
                (Ringing { deadline: later }, Event::Ended(EndReason::Busy), end(ending(None, Some(Outcome::Busy), Some("This contact is busy."))), None),
                (Ringing { deadline: later }, Event::Ended(EndReason::Declined), end(ending(None, Some(Outcome::Declined), Some("The call was declined."))), None),
                (Connected, Event::Ended(EndReason::Failed), end(ending(None, Some(Outcome::Failed), Some("The contact could not connect."))), None),
                (Incoming { deadline: later }, Event::AnsweredElsewhere, end(ending(None, None, None)), None),
                (connecting(Gathering(old)), Event::AnsweredElsewhere, end(ending(None, None, None)), None),
                (Ringing { deadline: later }, Event::AnsweredElsewhere, None, None),
                (Connected, Event::AnsweredElsewhere, None, None),
                (connecting(Established), Event::Connected, to(Connected, vec![]), None),
                (connecting(ApplyingAnswer(old)), Event::Connected, to(Connected, vec![]), None),
                (Reconnecting { deadline: later }, Event::Connected, to(Connected, vec![]), None),
                (Connected, Event::Connected, None, None),
                (Connected, Event::Disconnected, to(Reconnecting { deadline: now + RECONNECT }, vec![]), None),
                (Reconnecting { deadline: later }, Event::Disconnected, None, None),
                // A connection still being set up keeps its own, longer deadline.
                (connecting(Established), Event::Disconnected, None, None),
                (connecting(Established), Event::Lost, end(ending(Some(EndReason::Failed), Some(Outcome::Failed), Some(lost))), None),
                (Reconnecting { deadline: later }, Event::Lost, end(ending(Some(EndReason::Failed), Some(Outcome::Failed), Some(lost))), None),
            ];
            for (from, event, expected, wanted) in table {
                let label = format!("{from:?} on {event:?}");
                let (result, asked) = run(from, event, now, job);
                assert_eq!(result.map(|t| (t.effects, t.next)), expected, "{label}");
                assert_eq!(asked, wanted.into_iter().collect::<Vec<_>>(), "{label}");
            }
        });
    }

    #[test]
    fn a_job_that_cannot_start_ends_the_call_after_the_answer_is_recorded() {
        in_scope(|| {
            let now = Instant::now();
            let device = Uuid::new_v4();
            let result = transition(
                Stage::ringing(now),
                Event::Accepted { device },
                now,
                &mut |_| Err("no microphone".into()),
            )
            .unwrap();
            assert_eq!(
                result.effects,
                vec![Effect::Bind(device), Effect::Record(Outcome::Answered)]
            );
            assert_eq!(
                result.next,
                ending(
                    Some(EndReason::Failed),
                    Some(Outcome::Failed),
                    Some("no microphone")
                )
            );
            let refused = transition(
                Stage::Connecting {
                    deadline: now,
                    step: Negotiation::AwaitingOffer,
                },
                Event::Offer("v=0".into()),
                now,
                &mut |_| Err("no microphone".into()),
            )
            .unwrap();
            assert!(matches!(refused.next, Next::End(_)));
        });
    }

    #[test]
    fn only_connected_has_no_deadline_and_only_waiting_stages_hold_a_job() {
        in_scope(|| {
            let now = Instant::now();
            let job = spawn(async {});
            let stages = [
                (Stage::incoming(now), true, None),
                (
                    Stage::Switching {
                        deadline: now,
                        handoff: Some(job),
                    },
                    true,
                    Some(job),
                ),
                (
                    Stage::Switching {
                        deadline: now,
                        handoff: None,
                    },
                    true,
                    None,
                ),
                (Stage::ringing(now), true, None),
                (
                    Stage::Connecting {
                        deadline: now,
                        step: Negotiation::Gathering(job),
                    },
                    true,
                    Some(job),
                ),
                (
                    Stage::Connecting {
                        deadline: now,
                        step: Negotiation::AwaitingAnswer,
                    },
                    true,
                    None,
                ),
                (Stage::Connected, false, None),
                (Stage::Reconnecting { deadline: now }, true, None),
            ];
            for (stage, deadline, held) in stages {
                assert_eq!(stage.deadline().is_some(), deadline, "{stage:?}");
                assert_eq!(stage.job(), held, "{stage:?}");
            }
        });
    }

    impl Stage {
        fn with_deadline(self, deadline: Instant) -> Self {
            match self {
                Self::Connecting { step, .. } => Self::Connecting { deadline, step },
                other => other,
            }
        }
    }
}
