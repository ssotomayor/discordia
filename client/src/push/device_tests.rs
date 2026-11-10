use super::*;
use crate::push::registration::tests::{identity, sandbox, spawn_relay};
use crate::push::registration::{ensure_handle, set_category};

const PACKAGE: &str = "com.discordia.app";

fn phone(token: &str) -> Platform {
    Platform {
        token: Some(token.into()),
        enabled: true,
        package: PACKAGE.into(),
    }
}

fn relays() -> Vec<String> {
    vec!["wss://relay.example".into()]
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn registered_as(account: &str) -> PushState {
    PushState {
        account: Some(account.into()),
        relay: Some("https://push.example".into()),
        token: Some("old".into()),
        ..PushState::default()
    }
}

#[test]
fn what_the_relay_should_hold() {
    let me = "a".repeat(64);
    let fresh = PushState::default();
    let mine = registered_as(&me);
    let register = |dm| Want::Register {
        token: "tok".into(),
        package: PACKAGE.into(),
        dm,
        relays: relays(),
    };

    assert_eq!(
        wanted(&Platform::default(), &fresh, &me, &relays()),
        Want::Nothing
    );
    assert_eq!(
        wanted(&Platform::default(), &mine, &me, &relays()),
        Want::Unregister,
        "a token the platform withdrew"
    );
    assert_eq!(
        wanted(&phone("tok"), &fresh, &me, &relays()),
        register(true)
    );
    assert_eq!(wanted(&phone(""), &fresh, &me, &relays()), Want::Nothing);

    let disabled = Platform {
        enabled: false,
        ..phone("tok")
    };
    assert_eq!(wanted(&disabled, &fresh, &me, &relays()), Want::Nothing);
    assert_eq!(wanted(&disabled, &mine, &me, &relays()), Want::Unregister);
    assert_eq!(
        wanted(&disabled, &registered_as(&"b".repeat(64)), &me, &relays()),
        Want::Nothing,
        "another account's registration is not ours to drop"
    );

    let mut no_dms = mine.clone();
    no_dms.dms = false;
    assert_eq!(
        wanted(&phone("tok"), &no_dms, &me, &relays()),
        register(false)
    );
    no_dms.mentions = false;
    assert_eq!(
        wanted(&phone("tok"), &no_dms, &me, &relays()),
        Want::Unregister
    );
}

#[test]
fn only_a_registration_with_mentions_off_has_origins_to_revoke() {
    let me = "a".repeat(64);
    let mut state = registered_as(&me);
    state.servers.insert(
        "origin".into(),
        registration::ServerHandle {
            handle: "h".into(),
            key: "k".into(),
        },
    );
    assert!(stale_origins(&state, &me).is_empty());
    state.mentions = false;
    assert_eq!(stale_origins(&state, &me), vec!["origin".to_string()]);
    assert!(stale_origins(&state, &"b".repeat(64)).is_empty());
}

#[test]
fn a_registration_is_refreshed_once_a_day_and_at_once_when_it_changes() {
    let t0 = Instant::now();
    let want = wanted(&phone("tok"), &PushState::default(), "me", &relays());
    let mut schedule = Schedule::default();
    let run = |device: Option<Want>| {
        Plan::Run(Action {
            device,
            forget: vec![],
        })
    };

    assert_eq!(
        schedule.plan(&want, &[], t0),
        run(Some(want.clone())),
        "on mount"
    );
    schedule.record(&want, &[], true, t0);
    for later in [
        Duration::ZERO,
        Duration::from_secs(3600),
        REFRESH - Duration::from_secs(1),
    ] {
        assert_eq!(
            schedule.plan(&want, &[], t0 + later),
            Plan::At(t0 + REFRESH)
        );
    }
    assert_eq!(
        schedule.plan(&want, &[], t0 + REFRESH),
        run(Some(want.clone()))
    );

    let rotated = wanted(&phone("tok-2"), &PushState::default(), "me", &relays());
    assert_eq!(schedule.plan(&rotated, &[], t0), run(Some(rotated.clone())));
    assert_eq!(
        schedule.plan(&want, &["origin".into()], t0),
        Plan::Run(Action {
            device: None,
            forget: vec!["origin".into()]
        }),
        "revoking handles does not re-register"
    );
    assert_eq!(schedule.plan(&Want::Nothing, &[], t0), Plan::Idle);
    assert_eq!(
        schedule.plan(&Want::Unregister, &["origin".into()], t0),
        run(Some(Want::Unregister)),
        "unregistering drops every handle with the device"
    );
}

#[test]
fn a_failure_waits_out_a_growing_backoff_unless_the_inputs_change() {
    let t0 = Instant::now();
    let want = wanted(&phone("tok"), &PushState::default(), "me", &relays());
    let mut schedule = Schedule::default();
    schedule.record(&want, &[], false, t0);
    assert_eq!(schedule.plan(&want, &[], t0), Plan::At(t0 + FIRST_RETRY));
    schedule.record(&want, &[], false, t0 + FIRST_RETRY);
    assert_eq!(
        schedule.plan(&want, &[], t0 + FIRST_RETRY),
        Plan::At(t0 + FIRST_RETRY + FIRST_RETRY * 2)
    );
    let rotated = wanted(&phone("tok-2"), &PushState::default(), "me", &relays());
    assert!(matches!(schedule.plan(&rotated, &[], t0), Plan::Run(_)));
    assert_eq!(backoff(1), FIRST_RETRY);
    assert_eq!(backoff(40), LAST_RETRY);
}

fn ran_ok(outcome: Outcome) -> Action {
    match outcome {
        Outcome::Ran(action, Ok(())) => action,
        other => panic!("expected a successful call, got {other:?}"),
    }
}

#[tokio::test]
async fn mounting_with_a_token_registers_and_refreshes_once_a_day() {
    let (client, relay) = spawn_relay().await;
    let dir = sandbox();
    let me = identity(21);
    let mut device = Device::new(dir.clone(), client, me.clone());
    let t0 = Instant::now();

    assert!(matches!(
        device.step(&Platform::default(), &relays(), t0).await,
        Outcome::Idle
    ));
    assert_eq!(
        PushState::load(&dir),
        PushState::default(),
        "no token, no call"
    );

    ran_ok(device.step(&phone("tok"), &relays(), t0).await);
    assert!(relay.store.device_exists(&me.pubkey, "tok").unwrap());
    assert_eq!(
        relay
            .store
            .devices_of(&me.pubkey, true, now_unix())
            .unwrap()
            .len(),
        1
    );

    let hour = t0 + Duration::from_secs(3600);
    assert!(matches!(
        device.step(&phone("tok"), &relays(), hour).await,
        Outcome::At(at) if at == t0 + REFRESH
    ));
    relay.store.delete_device(&me.pubkey, "tok").unwrap();
    ran_ok(device.step(&phone("tok"), &relays(), t0 + REFRESH).await);
    assert!(
        relay.store.device_exists(&me.pubkey, "tok").unwrap(),
        "the daily PUT restores what the relay lost"
    );
}

#[tokio::test]
async fn the_dm_toggle_reregisters_and_both_off_unregisters() {
    let (client, relay) = spawn_relay().await;
    let dir = sandbox();
    let me = identity(22);
    let mut device = Device::new(dir.clone(), client, me.clone());
    let t0 = Instant::now();
    ran_ok(device.step(&phone("tok"), &relays(), t0).await);

    set_category(&dir, Category::Dms, false).await.unwrap();
    ran_ok(device.step(&phone("tok"), &relays(), t0).await);
    assert!(
        relay
            .store
            .devices_of(&me.pubkey, true, now_unix())
            .unwrap()
            .is_empty()
    );
    assert!(relay.store.device_exists(&me.pubkey, "tok").unwrap());

    set_category(&dir, Category::Mentions, false).await.unwrap();
    let action = ran_ok(device.step(&phone("tok"), &relays(), t0).await);
    assert_eq!(action.device, Some(Want::Unregister));
    assert!(!relay.store.device_exists(&me.pubkey, "tok").unwrap());
    assert!(PushState::load(&dir).account.is_none());
    assert!(matches!(
        device.step(&phone("tok"), &relays(), t0).await,
        Outcome::Idle
    ));

    set_category(&dir, Category::Dms, true).await.unwrap();
    ran_ok(device.step(&phone("tok"), &relays(), t0).await);
    assert!(relay.store.device_exists(&me.pubkey, "tok").unwrap());
}

#[tokio::test]
async fn a_rotated_token_keeps_the_handles_servers_hold() {
    let (client, relay) = spawn_relay().await;
    let dir = sandbox();
    let me = identity(23);
    let mut device = Device::new(dir.clone(), client.clone(), me.clone());
    let t0 = Instant::now();
    ran_ok(device.step(&phone("tok-1"), &relays(), t0).await);
    let (handle, key) = ensure_handle(&dir, &client, &me, "origin").await.unwrap();

    ran_ok(device.step(&phone("tok-2"), &relays(), t0).await);
    let target = relay
        .store
        .handle_target(&handle, now_unix())
        .unwrap()
        .unwrap();
    assert_eq!(target.token, "tok-2");
    assert!(!relay.store.device_exists(&me.pubkey, "tok-1").unwrap());
    assert_eq!(
        PushState::load(&dir).server_of(&handle),
        Some(("origin", key))
    );
}

#[tokio::test]
async fn switching_accounts_unregisters_the_old_key_and_registers_the_new() {
    let (client, relay) = spawn_relay().await;
    let dir = sandbox();
    let (old, new) = (identity(24), identity(25));
    let t0 = Instant::now();
    let mut first = Device::new(dir.clone(), client.clone(), old.clone());
    ran_ok(first.step(&phone("tok"), &relays(), t0).await);
    let (handle, _) = ensure_handle(&dir, &client, &old, "origin").await.unwrap();

    // `sign_out` dials the relay `relay_url()` names; here it is the test's.
    registration::unregister_device(&dir, &client, &old)
        .await
        .unwrap();
    assert!(!relay.store.device_exists(&old.pubkey, "tok").unwrap());
    assert!(
        relay
            .store
            .handle_target(&handle, now_unix())
            .unwrap()
            .is_none()
    );

    let mut second = Device::new(dir.clone(), client, new.clone());
    ran_ok(second.step(&phone("tok"), &relays(), t0).await);
    assert!(relay.store.device_exists(&new.pubkey, "tok").unwrap());
    let state = PushState::load(&dir);
    assert_eq!(state.account.as_deref(), Some(new.pubkey.as_str()));
    assert!(state.servers.is_empty());
}

#[tokio::test]
async fn sign_out_of_an_account_that_never_registered_touches_nothing() {
    let dir = sandbox();
    let other = registered_as(&identity(27).pubkey);
    other.save(&dir).unwrap();
    sign_out(&dir, &identity(26)).await;
    assert_eq!(PushState::load(&dir), other);
}

#[tokio::test]
async fn mentions_off_revokes_every_servers_handle() {
    let (client, relay) = spawn_relay().await;
    let dir = sandbox();
    let me = identity(28);
    let mut device = Device::new(dir.clone(), client.clone(), me.clone());
    let t0 = Instant::now();
    ran_ok(device.step(&phone("tok"), &relays(), t0).await);
    let (a, _) = ensure_handle(&dir, &client, &me, "origin-a").await.unwrap();
    let (b, _) = ensure_handle(&dir, &client, &me, "origin-b").await.unwrap();

    set_category(&dir, Category::Mentions, false).await.unwrap();
    let action = ran_ok(device.step(&phone("tok"), &relays(), t0).await);
    assert_eq!(action.device, None, "the device itself stays for DMs");
    assert_eq!(action.forget.len(), 2);
    assert!(PushState::load(&dir).servers.is_empty());
    for handle in [a, b] {
        assert!(
            relay
                .store
                .handle_target(&handle, now_unix())
                .unwrap()
                .is_none()
        );
    }
    assert!(relay.store.device_exists(&me.pubkey, "tok").unwrap());
    assert!(matches!(
        device.step(&phone("tok"), &relays(), t0).await,
        Outcome::At(_)
    ));
}
