//! A Ready session's side of mention push: give the server this install's
//! handle on every Ready, and take it back when mentions go off. Nothing here
//! may log a handle or a key.

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use tokio::sync::watch;

use crate::identity::Identity;
use crate::protocol::ClientMessage;
use crate::push::device;
use crate::push::registration::{self, PushState, RelayClient, same_relay};

/// A Ready session. `ready` counts Readies, so a reconnect registers afresh
/// even when nothing else about the session changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live {
    pub ready: u64,
    pub origin: String,
    /// `ServerInfo::push`.
    pub advertised: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Register,
    /// Mentions went off, or the device did: tell the server to stop.
    Withdraw(String),
    /// The server no longer pushes through our relay; revoke its handle there.
    Forget,
    Nothing,
}

/// `sent` is the handle given to this server since its last Ready.
pub fn step(
    state: &PushState,
    account: &str,
    relay: &str,
    live: &Live,
    sent: Option<&str>,
) -> Step {
    let ours = live
        .advertised
        .as_deref()
        .is_some_and(|advertised| same_relay(advertised, relay));
    if !ours {
        return if state.servers.contains_key(&live.origin) {
            Step::Forget
        } else {
            Step::Nothing
        };
    }
    let device = state.account.as_deref() == Some(account)
        && state.token.is_some()
        && state
            .relay
            .as_deref()
            .is_some_and(|registered| same_relay(registered, relay));
    match sent {
        Some(handle) if !(device && state.mentions) => Step::Withdraw(handle.to_owned()),
        None if device && state.mentions => Step::Register,
        _ => Step::Nothing,
    }
}

const FIRST_RETRY: Duration = Duration::from_secs(10);
const LAST_RETRY: Duration = Duration::from_secs(10 * 60);

/// One session's loop: `live` is `None` outside Ready.
pub async fn run(
    dir: PathBuf,
    relay: RelayClient,
    identity: Identity,
    mut live: watch::Receiver<Option<Live>>,
    send: impl Fn(ClientMessage),
) {
    let mut changes = device::changes().subscribe();
    let mut sent: Option<(u64, String)> = None;
    let mut failures = 0u32;
    loop {
        let current = live.borrow_and_update().clone();
        changes.borrow_and_update();
        let mut retry = None;
        if let Some(session) = current {
            if sent
                .as_ref()
                .is_some_and(|(ready, _)| *ready != session.ready)
            {
                sent = None;
            }
            let state = PushState::load(&dir);
            let given = sent.as_ref().map(|(_, handle)| handle.as_str());
            let result = match step(&state, &identity.pubkey, relay.url(), &session, given) {
                Step::Register => {
                    registration::ensure_handle(&dir, &relay, &identity, &session.origin)
                        .await
                        .map(|(handle, key)| {
                            send(ClientMessage::RegisterPush {
                                handle: handle.clone(),
                                key: base64::engine::general_purpose::STANDARD.encode(key),
                            });
                            sent = Some((session.ready, handle));
                        })
                }
                Step::Withdraw(handle) => {
                    send(ClientMessage::UnregisterPush { handle });
                    sent = None;
                    Ok(())
                }
                Step::Forget => {
                    sent = None;
                    registration::forget_origin(&dir, &relay, &identity, &session.origin).await
                }
                Step::Nothing => Ok(()),
            };
            match result {
                Ok(()) => failures = 0,
                Err(error) => {
                    tracing::warn!(%error, "push: this server was not given a handle");
                    failures = failures.saturating_add(1);
                    retry = Some(
                        FIRST_RETRY
                            .saturating_mul(1u32 << failures.saturating_sub(1).min(8))
                            .min(LAST_RETRY),
                    );
                }
            }
        } else {
            sent = None;
        }
        let sleep = async move {
            match retry {
                Some(after) => tokio::time::sleep(after).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            changed = live.changed() => if changed.is_err() { return },
            _ = changes.changed() => {}
            _ = sleep => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::registration::ServerHandle;

    const RELAY: &str = "https://push.discordia.world";
    const ORIGIN: &str = "chat.example:443";

    fn me() -> String {
        "a".repeat(64)
    }

    fn registered() -> PushState {
        PushState {
            account: Some(me()),
            relay: Some(RELAY.into()),
            token: Some("tok".into()),
            ..PushState::default()
        }
    }

    fn with_handle(mut state: PushState) -> PushState {
        state.servers.insert(
            ORIGIN.into(),
            ServerHandle {
                handle: "h".into(),
                key: "k".into(),
            },
        );
        state
    }

    fn live(advertised: Option<&str>) -> Live {
        Live {
            ready: 1,
            origin: ORIGIN.into(),
            advertised: advertised.map(str::to_owned),
        }
    }

    #[test]
    fn a_ready_registers_only_where_the_server_pushes_through_our_relay() {
        let state = registered();
        for advertised in [
            "https://push.discordia.world",
            "https://push.discordia.world/",
            "HTTPS://PUSH.discordia.world",
        ] {
            assert_eq!(
                step(&state, &me(), RELAY, &live(Some(advertised)), None),
                Step::Register,
                "{advertised}"
            );
        }
        for advertised in [
            None,
            Some("https://push.other.example"),
            Some("http://push.discordia.world"),
            Some("not a url"),
        ] {
            assert_eq!(
                step(&state, &me(), RELAY, &live(advertised), None),
                Step::Nothing,
                "{advertised:?}"
            );
            assert_eq!(
                step(
                    &with_handle(registered()),
                    &me(),
                    RELAY,
                    &live(advertised),
                    None
                ),
                Step::Forget,
                "a server that stopped advertising our relay: {advertised:?}"
            );
        }
    }

    #[test]
    fn a_ready_registers_only_a_registered_device_with_mentions_on() {
        let ready = live(Some(RELAY));
        assert_eq!(
            step(&PushState::default(), &me(), RELAY, &ready, None),
            Step::Nothing,
            "no device (every desktop)"
        );
        let mut other_account = registered();
        other_account.account = Some("b".repeat(64));
        assert_eq!(
            step(&other_account, &me(), RELAY, &ready, None),
            Step::Nothing
        );
        let mut other_relay = registered();
        other_relay.relay = Some("https://push.other.example".into());
        assert_eq!(
            step(&other_relay, &me(), RELAY, &ready, None),
            Step::Nothing
        );
        let mut no_token = registered();
        no_token.token = None;
        assert_eq!(step(&no_token, &me(), RELAY, &ready, None), Step::Nothing);

        assert_eq!(
            step(&registered(), &me(), RELAY, &ready, Some("h")),
            Step::Nothing
        );
    }

    async fn next(out: &mut tokio::sync::mpsc::UnboundedReceiver<ClientMessage>) -> ClientMessage {
        tokio::time::timeout(Duration::from_secs(5), out.recv())
            .await
            .expect("the loop sent nothing")
            .unwrap()
    }

    #[tokio::test]
    async fn every_ready_gives_the_server_the_handle_until_mentions_go_off() {
        use crate::push::registration::tests::{identity, sandbox, spawn_relay};
        use crate::push::registration::{Category, register_device, set_category};

        let (client, relay) = spawn_relay().await;
        let dir = sandbox();
        let me = identity(31);
        register_device(&dir, &client, &me, "tok", "pkg", true, vec![])
            .await
            .unwrap();
        let (live, receiver) = watch::channel(None);
        let (out_tx, mut out) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(run(
            dir.clone(),
            client.clone(),
            me.clone(),
            receiver,
            move |m| {
                let _ = out_tx.send(m);
            },
        ));
        let session = |ready, advertised: &str| {
            Some(Live {
                ready,
                origin: ORIGIN.into(),
                advertised: Some(advertised.into()),
            })
        };

        live.send_replace(session(1, &format!("{}/", client.url())));
        let ClientMessage::RegisterPush { handle, key } = next(&mut out).await else {
            panic!("expected RegisterPush");
        };
        let key: [u8; 32] = base64::engine::general_purpose::STANDARD
            .decode(&key)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            PushState::load(&dir).server_of(&handle),
            Some((ORIGIN, key))
        );
        assert!(relay.store.handle_target(&handle, 0).unwrap().is_some());

        live.send_replace(None);
        live.send_replace(session(2, client.url()));
        assert!(
            matches!(next(&mut out).await, ClientMessage::RegisterPush { handle: again, .. } if again == handle),
            "a reconnect refreshes the same handle"
        );

        set_category(&dir, Category::Mentions, false).await.unwrap();
        device::changed();
        assert!(matches!(
            next(&mut out).await,
            ClientMessage::UnregisterPush { handle: gone } if gone == handle
        ));

        live.send_replace(session(3, "https://push.other.example"));
        tokio::time::timeout(Duration::from_secs(5), async {
            while PushState::load(&dir).servers.contains_key(ORIGIN) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a server on another relay keeps no handle");
        assert!(out.try_recv().is_err(), "and is sent nothing");
        task.abort();
    }

    #[test]
    fn mentions_or_the_device_going_off_withdraws_what_this_session_gave() {
        let ready = live(Some(RELAY));
        let mut off = registered();
        off.mentions = false;
        assert_eq!(
            step(&off, &me(), RELAY, &ready, Some("h")),
            Step::Withdraw("h".into())
        );
        assert_eq!(step(&off, &me(), RELAY, &ready, None), Step::Nothing);
        let mut gone = registered();
        gone.account = None;
        assert_eq!(
            step(&gone, &me(), RELAY, &ready, Some("h")),
            Step::Withdraw("h".into())
        );
    }
}
