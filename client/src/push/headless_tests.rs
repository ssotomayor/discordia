use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use base64::Engine as _;
use secp256k1::SecretKey;

use super::*;
use crate::identity::Identity;
use crate::nostr::event::{self, xonly_hex};
use crate::nostr::{call_history, calls};
use crate::profile::LocalProfile;
use crate::push::peers::Peer;
use crate::push::registration::ServerHandle;

const NOW: i64 = 1_700_000_000;
const BODY: &str = "the secret body nobody else may read";
const ORIGIN: &str = "quic://host@203.0.113.7:4433";
const HANDLE: &str = "aGFuZGxlLWZvci10aGUtdGVzdA";
const MENTION_KEY: [u8; 32] = [7; 32];

struct Fixture {
    dir: PathBuf,
    me: Identity,
    peer: SecretKey,
    passphrase: String,
}

fn secret(seed: u8) -> SecretKey {
    SecretKey::from_slice(&[seed; 32]).unwrap()
}

/// Written through the app's own savers, under the test thread's config dir.
fn fixture() -> Fixture {
    let dir = crate::identity::test_config_dir();
    let me = Identity::restore_from_private_key(hex::encode([0x11; 32]), "me").unwrap();
    me.save().unwrap();
    let passphrase = crate::keyvault::passphrase().unwrap().to_string();
    let peer = secret(0x22);
    let f = Fixture {
        dir,
        me,
        peer,
        passphrase,
    };
    f.settings(|s| s.dm_accepted = vec![f.peer_key()]);
    crate::profile::save(&f.me.pubkey, &LocalProfile::default()).unwrap();
    crate::push::peers::write(
        &f.dir,
        &PushPeers {
            account: f.me.pubkey.clone(),
            peers: vec![Peer {
                pubkey: xonly_hex(&secret(0x33)),
                name: "Con\u{7}tact".into(),
            }],
        },
    )
    .unwrap();
    PushState {
        account: Some(f.me.pubkey.clone()),
        relay: Some("https://push.example".into()),
        token: Some("token".into()),
        servers: [(
            ORIGIN.to_string(),
            ServerHandle {
                handle: HANDLE.into(),
                key: base64::engine::general_purpose::STANDARD.encode(MENTION_KEY),
            },
        )]
        .into(),
        ..PushState::default()
    }
    .save(&f.dir)
    .unwrap();
    f
}

impl Fixture {
    fn peer_key(&self) -> String {
        xonly_hex(&self.peer)
    }

    fn settings(&self, change: impl FnOnce(&mut ClientSettings)) {
        let mut settings = ClientSettings::default();
        change(&mut settings);
        crate::settings::save(&settings);
    }

    fn dnd(&self) {
        crate::profile::save(
            &self.me.pubkey,
            &LocalProfile {
                status: Some("dnd".into()),
                ..Default::default()
            },
        )
        .unwrap();
    }

    fn chat_from(&self, from: &SecretKey, at: i64) -> Rumor {
        nip17::chat_rumor(&xonly_hex(from), &self.me.pubkey, BODY, None, &[], at)
    }

    fn wrap(&self, from: &SecretKey, rumor: &Rumor) -> Event {
        nip59::wrap(from, &self.me.pubkey, rumor, NOW).unwrap()
    }

    fn inline(&self, gift: &Event) -> String {
        dm_data(&self.me.pubkey, gift)
    }

    /// The alert, and whether the key was unlocked to get it.
    fn run(&self, data: &str) -> (Option<Alert>, bool) {
        self.run_with(data, &self.passphrase)
    }

    fn run_with(&self, data: &str, passphrase: &str) -> (Option<Alert>, bool) {
        let unlocked = Cell::new(false);
        let alert = handle_push(
            &self.dir,
            data,
            "en-US",
            NOW,
            || {
                unlocked.set(true);
                Ok(passphrase.to_string())
            },
            |_, _| Err("no fetch expected".into()),
        );
        (alert, unlocked.get())
    }

    fn clean_dm(&self) -> String {
        self.inline(&self.wrap(&self.peer, &self.chat_from(&self.peer, NOW - 10)))
    }
}

fn dm_data(p: &str, gift: &Event) -> String {
    serde_json::to_string(&PushData::Dm {
        p: p.into(),
        wrap: DmWrap::Inline {
            e: serde_json::to_string(gift).unwrap(),
        },
    })
    .unwrap()
}

fn none_after_unlock(f: &Fixture, data: &str) {
    assert_eq!(f.run(data), (None, true));
}

fn none_before_unlock(f: &Fixture, data: &str) {
    assert_eq!(f.run(data), (None, false));
}

#[test]
fn a_clean_dm_from_an_inbox_peer_alerts_with_its_text_and_a_public_line_without_it() {
    let f = fixture();
    let rumor = f.chat_from(&f.peer, NOW - 10);
    let (alert, unlocked) = f.run(&f.inline(&f.wrap(&f.peer, &rumor)));
    assert!(unlocked);
    assert_eq!(
        alert.expect("a clean DM alerts"),
        Alert::Dm {
            account: f.me.pubkey.clone(),
            peer: f.peer_key(),
            sender: crate::identity::truncate_pubkey(&f.peer_key()),
            message: BODY.into(),
            public: "New private message".into(),
            silent: false,
            at: NOW - 10,
            id: rumor.id.clone(),
        }
    );
}

#[test]
fn with_the_text_setting_off_the_alert_carries_no_body() {
    let f = fixture();
    f.settings(|settings| {
        settings.dm_accepted = vec![f.peer_key()];
        settings.dm_notification_text = false;
    });
    let rumor = f.chat_from(&f.peer, NOW - 10);
    let (alert, _) = f.run(&f.inline(&f.wrap(&f.peer, &rumor)));
    let json = serde_json::to_string(&alert.expect("a clean DM alerts")).unwrap();
    assert!(json.contains("New private message"));
    assert!(!json.contains(BODY) && !json.contains("secret body"));
}

#[test]
fn the_alert_json_is_the_shape_kotlin_reads() {
    let dm = Alert::Dm {
        account: "a".into(),
        peer: "p".into(),
        sender: "s".into(),
        message: "m".into(),
        public: "g".into(),
        silent: true,
        at: 5,
        id: "i".into(),
    };
    assert_eq!(
        serde_json::to_value(&dm).unwrap(),
        serde_json::json!({"kind": "dm", "account": "a", "peer": "p", "sender": "s",
            "message": "m", "public": "g", "silent": true, "at": 5, "id": "i"})
    );
    let id = Id::from_u128(1);
    let mention = Alert::Mention {
        account: "a".into(),
        origin: "o".into(),
        guild: id,
        channel: id,
        message_id: id,
        author: "k".into(),
        author_name: "n".into(),
        guild_name: "g".into(),
        channel_name: "c".into(),
        text: "t".into(),
        silent: false,
        at: 6,
    };
    let id = "00000000-0000-0000-0000-000000000001";
    assert_eq!(
        serde_json::to_value(&mention).unwrap(),
        serde_json::json!({"kind": "mention", "account": "a", "origin": "o", "guild": id,
            "channel": id, "message_id": id, "author": "k", "author_name": "n",
            "guild_name": "g", "channel_name": "c", "text": "t", "silent": false, "at": 6})
    );
    assert_eq!(
        serde_json::to_value(Alert::Ping).unwrap(),
        serde_json::json!({"kind": "ping"})
    );
}

#[test]
fn a_contact_from_the_peer_cache_alerts_under_its_cached_name() {
    let f = fixture();
    let contact = secret(0x33);
    let (alert, _) = f.run(&f.inline(&f.wrap(&contact, &f.chat_from(&contact, NOW - 10))));
    match alert {
        Some(Alert::Dm { sender, .. }) => assert_eq!(sender, "Contact"),
        other => panic!("expected a DM alert, got {other:?}"),
    }
}

#[test]
fn the_language_and_a_silenced_device_carry_through() {
    let f = fixture();
    let text = |f: &Fixture, system: &str| {
        let alert = handle_push(
            &f.dir,
            &f.clean_dm(),
            system,
            NOW,
            || Ok::<_, String>(f.passphrase.clone()),
            |_, _| Err("unused".into()),
        );
        match alert {
            Some(Alert::Dm { public, silent, .. }) => (public, silent),
            other => panic!("expected a DM alert, got {other:?}"),
        }
    };
    assert_eq!(text(&f, "es-AR"), ("Nuevo mensaje privado".into(), false));
    f.settings(|s| {
        s.dm_accepted = vec![f.peer_key()];
        s.language = crate::i18n::Language::Portuguese;
        s.sfx_volume = 0;
    });
    assert_eq!(text(&f, "es-AR"), ("Nova mensagem privada".into(), true));
}

#[test]
fn a_self_copy_from_another_device_is_quiet() {
    let f = fixture();
    let mine = nip17::chat_rumor(&f.me.pubkey, &f.peer_key(), BODY, None, &[], NOW - 10);
    let gift = nip59::wrap(&f.me.secret_key(), &f.me.pubkey, &mine, NOW).unwrap();
    none_after_unlock(&f, &f.inline(&gift));
}

#[test]
fn blocked_ignored_and_unknown_senders_are_quiet() {
    let f = fixture();
    f.settings(|s| {
        s.dm_accepted = vec![f.peer_key()];
        s.dm_blocked = vec![f.peer_key()];
    });
    none_after_unlock(&f, &f.clean_dm());

    f.settings(|s| s.dm_ignored = vec![f.peer_key()]);
    none_after_unlock(&f, &f.clean_dm());

    f.settings(|_| {});
    none_after_unlock(&f, &f.clean_dm());
}

#[test]
fn a_muted_conversation_is_quiet() {
    let f = fixture();
    f.settings(|s| {
        s.dm_accepted = vec![f.peer_key()];
        s.muted_channels = vec![crate::nostr::service::conversation_id(&f.peer_key())];
    });
    none_after_unlock(&f, &f.clean_dm());
}

#[test]
fn do_not_disturb_is_quiet_without_unlocking_the_key() {
    let f = fixture();
    f.dnd();
    none_before_unlock(&f, &f.clean_dm());
}

#[test]
fn a_read_message_is_quiet_in_the_senders_clock() {
    let f = fixture();
    let peer = f.peer_key();
    f.settings(|s| {
        s.dm_accepted = vec![peer.clone()];
        s.dm_read_at = vec![(peer.clone(), NOW)];
    });
    none_after_unlock(&f, &f.clean_dm());

    // Read up to NOW - 20 on our clock; the sender runs 15 s fast, so NOW - 10 is ours NOW - 25.
    f.settings(|s| {
        s.dm_accepted = vec![peer.clone()];
        s.dm_read_at = vec![(peer.clone(), NOW - 20)];
    });
    assert!(f.run(&f.clean_dm()).0.is_some());
    f.settings(|s| {
        s.dm_accepted = vec![peer.clone()];
        s.dm_read_at = vec![(peer.clone(), NOW - 20)];
        s.dm_clock_offset = vec![(peer.clone(), 15)];
    });
    none_after_unlock(&f, &f.clean_dm());
}

#[test]
fn a_deleted_message_is_quiet() {
    let f = fixture();
    let rumor = f.chat_from(&f.peer, NOW - 10);
    f.settings(|s| {
        s.dm_accepted = vec![f.peer_key()];
        s.dm_deleted_ids = vec![rumor.id.clone()];
    });
    none_after_unlock(&f, &f.inline(&f.wrap(&f.peer, &rumor)));
}

#[test]
fn call_signals_records_edits_and_deletions_are_quiet() {
    let f = fixture();
    let peer = f.peer_key();
    let me = f.me.pubkey.clone();
    let signal = event::rumor(
        &peer,
        NOW - 10,
        calls::KIND_CALL,
        vec![vec!["p".into(), me.clone()]],
        "{}".into(),
    );
    let record = call_history::rumor(
        &peer,
        &call_history::Record {
            version: 1,
            call_id: uuid::Uuid::new_v4(),
            caller: peer.clone(),
            callee: me.clone(),
            started_at: NOW - 20,
            outcome: call_history::Outcome::Missed,
        },
        NOW - 10,
    )
    .unwrap();
    let target = "ab".repeat(32);
    let edit = nip17::chat_edit_rumor(&peer, &me, &target, "edited", &[], NOW - 10);
    let deletion = nip17::chat_deletion_rumor(&peer, &me, &target, NOW - 10);
    let expiring = nip59::wrap_with_expiration(&f.peer, &me, &signal, NOW, Some(NOW + 60)).unwrap();
    none_before_unlock(&f, &f.inline(&expiring));
    for rumor in [signal, record, edit, deletion] {
        none_after_unlock(&f, &f.inline(&f.wrap(&f.peer, &rumor)));
    }
}

#[test]
fn a_wrap_for_another_account_never_unlocks_the_key() {
    let f = fixture();
    let other = xonly_hex(&secret(0x44));
    let rumor = nip17::chat_rumor(&f.peer_key(), &other, BODY, None, &[], NOW - 10);
    let theirs = nip59::wrap(&f.peer, &other, &rumor, NOW).unwrap();
    none_before_unlock(&f, &dm_data(&f.me.pubkey, &theirs));
    none_before_unlock(&f, &dm_data(&other, &theirs));
    let ours = f.wrap(&f.peer, &f.chat_from(&f.peer, NOW - 10));
    none_before_unlock(&f, &dm_data(&other, &ours));
}

#[test]
fn a_wrong_passphrase_alerts_nothing() {
    let f = fixture();
    assert_eq!(
        f.run_with(&f.clean_dm(), "not the passphrase"),
        (None, true)
    );
    let failed = handle_push(
        &f.dir,
        &f.clean_dm(),
        "en",
        NOW,
        || Err("keystore unavailable".into()),
        |_, _| Err("unused".into()),
    );
    assert_eq!(failed, None);
}

#[test]
fn garbage_truncated_or_forged_events_never_unlock_the_key() {
    let f = fixture();
    let gift = f.wrap(&f.peer, &f.chat_from(&f.peer, NOW - 10));
    let json = serde_json::to_string(&gift).unwrap();
    let mut forged = gift.clone();
    forged.sig = format!("{}00", &forged.sig[..forged.sig.len() - 2]);
    let mut altered = gift.clone();
    altered.content.push('A');
    let mut not_a_wrap = gift.clone();
    not_a_wrap.kind = 14;
    for e in [
        String::new(),
        "{".into(),
        "null".into(),
        json[..json.len() / 2].to_string(),
        serde_json::to_string(&forged).unwrap(),
        serde_json::to_string(&altered).unwrap(),
        serde_json::to_string(&not_a_wrap).unwrap(),
    ] {
        let data = serde_json::json!({"t": "dm", "p": f.me.pubkey, "e": e}).to_string();
        none_before_unlock(&f, &data);
    }
}

#[test]
fn a_message_older_than_two_days_is_quiet() {
    let f = fixture();
    let old = f.chat_from(&f.peer, NOW - MAX_AGE_SECS - 1);
    none_after_unlock(&f, &f.inline(&f.wrap(&f.peer, &old)));
}

#[test]
fn a_large_wrap_is_fetched_by_id_and_must_be_the_one_asked_for() {
    let f = fixture();
    let gift = f.wrap(&f.peer, &f.chat_from(&f.peer, NOW - 10));
    let other = f.wrap(&f.peer, &f.chat_from(&f.peer, NOW - 11));
    let by_id = serde_json::json!({"t": "dm", "p": f.me.pubkey, "id": gift.id,
        "r": "wss://relay.example"})
    .to_string();
    let asked = RefCell::new(None);
    let serve = |event: &Event| {
        let asked = &asked;
        let json = serde_json::to_string(event).unwrap();
        move |id: &str, relay: &str| {
            *asked.borrow_mut() = Some((id.to_string(), relay.to_string()));
            Ok::<_, String>(json)
        }
    };
    let pass = || Ok::<_, String>(f.passphrase.clone());
    assert!(matches!(
        handle_push(&f.dir, &by_id, "en", NOW, pass, serve(&gift)),
        Some(Alert::Dm { .. })
    ));
    assert_eq!(
        asked.take(),
        Some((gift.id.clone(), "wss://relay.example".to_string()))
    );
    let unlocked = Cell::new(false);
    let wrong = handle_push(
        &f.dir,
        &by_id,
        "en",
        NOW,
        || {
            unlocked.set(true);
            Ok(f.passphrase.clone())
        },
        serve(&other),
    );
    assert_eq!(wrong, None);
    assert!(!unlocked.get());

    let bad_id = serde_json::json!({"t": "dm", "p": f.me.pubkey, "id": "nope",
        "r": "wss://relay.example"})
    .to_string();
    let called = Cell::new(false);
    let none = handle_push(&f.dir, &bad_id, "en", NOW, pass, |_, _| {
        called.set(true);
        Err("unused".into())
    });
    assert_eq!(none, None);
    assert!(!called.get());
}

fn payload(at: i64) -> MentionPayload {
    MentionPayload {
        v: 1,
        k: PushReason::Mention,
        guild: Id::from_u128(10),
        channel: Id::from_u128(20),
        message: Id::from_u128(30),
        author: xonly_hex(&secret(0x55)),
        author_name: "Ana\u{202e}\n".into(),
        guild_name: "Guild".into(),
        channel_name: "general".into(),
        at,
    }
}

fn mention_data(handle: &str, key: &[u8; 32], payload: &serde_json::Value) -> String {
    let c = nip44::encrypt(key, &payload.to_string()).unwrap();
    serde_json::to_string(&PushData::Mention {
        h: handle.into(),
        c,
    })
    .unwrap()
}

fn clean_mention() -> String {
    mention_data(
        HANDLE,
        &MENTION_KEY,
        &serde_json::to_value(payload(NOW - 5)).unwrap(),
    )
}

#[test]
fn a_mention_under_the_handles_key_alerts() {
    let f = fixture();
    let (alert, unlocked) = f.run(&clean_mention());
    assert!(!unlocked, "a mention never needs the identity key");
    assert_eq!(
        alert,
        Some(Alert::Mention {
            account: f.me.pubkey.clone(),
            origin: ORIGIN.into(),
            guild: Id::from_u128(10),
            channel: Id::from_u128(20),
            message_id: Id::from_u128(30),
            author: xonly_hex(&secret(0x55)),
            author_name: "Ana\u{202e}".into(),
            guild_name: "Guild".into(),
            channel_name: "general".into(),
            text: "\u{2068}Ana\u{202e}\u{2069} mentioned you".into(),
            silent: false,
            at: NOW - 5,
        })
    );
}

#[test]
fn the_mention_line_is_in_the_persons_language_and_names_a_nameless_author() {
    let f = fixture();
    let text = |f: &Fixture, system: &str, payload: MentionPayload| {
        let data = mention_data(
            HANDLE,
            &MENTION_KEY,
            &serde_json::to_value(payload).unwrap(),
        );
        let alert = handle_push(
            &f.dir,
            &data,
            system,
            NOW,
            || Err::<String, _>("a mention never unlocks".into()),
            |_, _| Err("unused".into()),
        );
        match alert {
            Some(Alert::Mention { text, .. }) => text,
            other => panic!("expected a mention alert, got {other:?}"),
        }
    };
    assert_eq!(
        text(&f, "es-AR", payload(NOW - 5)),
        "\u{2068}Ana\u{202e}\u{2069} te mencionó"
    );
    f.settings(|s| s.language = crate::i18n::Language::Portuguese);
    let nameless = MentionPayload {
        author_name: " \n".into(),
        ..payload(NOW - 5)
    };
    let short = crate::identity::truncate_pubkey(&xonly_hex(&secret(0x55)));
    assert_eq!(
        text(&f, "es-AR", nameless),
        format!("\u{2068}{short}\u{2069} mencionou você")
    );
}

#[test]
fn a_mention_that_does_not_open_is_quiet() {
    let f = fixture();
    let good = serde_json::to_value(payload(NOW - 5)).unwrap();
    assert_eq!(f.run(&mention_data(HANDLE, &[8; 32], &good)).0, None);
    assert_eq!(f.run(&mention_data("unknown", &MENTION_KEY, &good)).0, None);

    let c = nip44::encrypt(&MENTION_KEY, &good.to_string()).unwrap();
    let mut raw = base64::engine::general_purpose::STANDARD
        .decode(&c)
        .unwrap();
    raw[40] ^= 1;
    let tampered = base64::engine::general_purpose::STANDARD.encode(raw);
    let data = serde_json::json!({"t": "m", "h": HANDLE, "c": tampered}).to_string();
    assert_eq!(f.run(&data).0, None);

    for (field, value) in [
        ("k", serde_json::json!("reply")),
        ("v", serde_json::json!(2)),
        ("author", serde_json::json!(f.me.pubkey)),
        ("author", serde_json::json!("not a key")),
    ] {
        let mut changed = good.clone();
        changed[field] = value;
        assert_eq!(f.run(&mention_data(HANDLE, &MENTION_KEY, &changed)).0, None);
    }
}

#[test]
fn a_category_switched_off_is_quiet_and_never_unlocks_the_key() {
    use crate::push::registration::Category;
    let f = fixture();
    let set = |category, on| {
        let mut state = PushState::load(&f.dir);
        state.set(category, on);
        state.save(&f.dir).unwrap();
    };
    set(Category::Dms, false);
    none_before_unlock(&f, &f.clean_dm());
    assert!(f.run(&clean_mention()).0.is_some(), "mentions are separate");

    set(Category::Dms, true);
    set(Category::Mentions, false);
    assert_eq!(f.run(&clean_mention()), (None, false));
    assert!(f.run(&f.clean_dm()).0.is_some(), "DMs are separate");

    set(Category::Mentions, true);
    assert!(f.run(&clean_mention()).0.is_some());
}

#[test]
fn a_mention_for_a_signed_out_account_is_quiet() {
    let f = fixture();
    let mut state = PushState::load(&f.dir);
    state.account = Some(xonly_hex(&secret(0x66)));
    state.save(&f.dir).unwrap();
    assert_eq!(f.run(&clean_mention()).0, None);
}

#[test]
fn muted_dnd_or_read_mentions_are_quiet() {
    let f = fixture();
    f.settings(|s| s.muted_channels = vec![Id::from_u128(20)]);
    assert_eq!(f.run(&clean_mention()).0, None);
    f.settings(|s| s.muted_guilds = vec![Id::from_u128(10)]);
    assert_eq!(f.run(&clean_mention()).0, None);

    f.settings(|s| {
        s.channel_read_at
            .entry(ORIGIN.into())
            .or_default()
            .insert(Id::from_u128(20), (NOW - 5) * 1000);
    });
    assert_eq!(f.run(&clean_mention()).0, None);
    f.settings(|s| {
        s.channel_read_at
            .entry("quic://another@198.51.100.1:1".into())
            .or_default()
            .insert(Id::from_u128(20), NOW * 1000);
        s.channel_read_at
            .entry(ORIGIN.into())
            .or_default()
            .insert(Id::from_u128(20), (NOW - 5) * 1000 - 1);
    });
    assert!(
        f.run(&clean_mention()).0.is_some(),
        "another server's mark is another channel"
    );

    f.settings(|_| {});
    f.dnd();
    assert_eq!(f.run(&clean_mention()).0, None);
}

#[test]
fn ping_and_unknown_kinds() {
    let f = fixture();
    assert_eq!(f.run(r#"{"t":"ping"}"#), (Some(Alert::Ping), false));
    assert_eq!(f.run(r#"{"t":"call","x":"y"}"#), (None, false));
}

#[test]
fn nothing_the_handler_is_given_makes_it_panic() {
    let f = fixture();
    let real = [f.clean_dm(), clean_mention()];
    let mut inputs: Vec<String> = [
        "",
        " ",
        "{",
        "}",
        "[]",
        "null",
        "0",
        "\"t\"",
        "{\"t\":5}",
        "{\"t\":null}",
        "{\"t\":\"dm\"}",
        "{\"t\":\"dm\",\"p\":\"\"}",
        "{\"t\":\"dm\",\"p\":7,\"e\":{}}",
        "{\"t\":\"dm\",\"p\":\"x\",\"id\":\"\",\"r\":\"\"}",
        "{\"t\":\"m\",\"h\":\"\",\"c\":\"\"}",
        "{\"t\":\"m\",\"h\":\"aGFuZGxlLWZvci10aGUtdGVzdA\",\"c\":\"#\"}",
        "{\"t\":\"m\",\"h\":\"aGFuZGxlLWZvci10aGUtdGVzdA\",\"c\":\"AAAA\"}",
        "{\"t\":\"ping\",\"t\":\"dm\"}",
        "\u{0}\u{ffff}",
        "{\"t\":\"dm\",\"p\":\"é\"}",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    inputs.push("[".repeat(10_000));
    inputs.push("x".repeat(MAX_DATA_BYTES + 1));
    for data in &real {
        let mut cut = 0;
        while cut < data.len() {
            if data.is_char_boundary(cut) {
                inputs.push(data[..cut].to_string());
            }
            cut += 7;
        }
    }
    let gift = serde_json::to_string(&f.wrap(&f.peer, &f.chat_from(&f.peer, NOW))).unwrap();
    for cut in (0..gift.len()).step_by(53) {
        inputs
            .push(serde_json::json!({"t": "dm", "p": f.me.pubkey, "e": &gift[..cut]}).to_string());
    }
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    for _ in 0..200 {
        let len = (seed % 300) as usize;
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 24) as u8
            })
            .collect();
        inputs.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    for data in &inputs {
        let _ = handle_push(
            &f.dir,
            data,
            "\u{0}",
            i64::MIN,
            || Ok(f.passphrase.clone()),
            |_, _| Ok("{\"garbage\":".into()),
        );
        let _ = handle_push(
            &f.dir,
            data,
            "",
            i64::MAX,
            || Err(String::new()),
            |_, _| Err(String::new()),
        );
    }
}

#[test]
fn a_damaged_config_folder_reads_as_no_alert() {
    let f = fixture();
    for (file, junk) in [
        ("identity.json", "{\"version\":2,\"active\":\"ZZ\"}"),
        ("settings.json", "{not json"),
        ("push.json", "[]"),
        ("push-peers.json", "7"),
    ] {
        std::fs::write(f.dir.join(file), junk).unwrap();
        let _ = f.run(&f.clean_dm());
        let _ = f.run(&clean_mention());
    }
    assert_eq!(f.run(&f.clean_dm()), (None, false));
    let empty = std::env::temp_dir().join(format!("discordia-empty-{}", uuid::Uuid::new_v4()));
    assert_eq!(
        handle_push(
            &empty,
            &f.clean_dm(),
            "en",
            NOW,
            || Ok(String::new()),
            |_, _| Err(String::new())
        ),
        None
    );
}

#[test]
fn fetch_refuses_anything_but_wss_and_an_event_id() {
    let id = "ab".repeat(32);
    assert!(fetch_event_blocking(&id, "ws://relay.example").is_err());
    assert!(fetch_event_blocking(&id, "https://relay.example").is_err());
    assert!(fetch_event_blocking("abc", "wss://relay.example").is_err());
}

#[test]
fn fetch_returns_the_event_a_relay_holds_and_nothing_else() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let wanted = "cd".repeat(32);
    let event = serde_json::json!({"id": wanted, "kind": 1059});
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    let served = event.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            addr_tx.send(listener.local_addr().unwrap()).unwrap();
            for _ in 0..2 {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                let Some(Ok(Message::Text(req))) = ws.next().await else {
                    continue;
                };
                let req: serde_json::Value = serde_json::from_str(&req).unwrap();
                let sub = req[1].clone();
                let asked = req[2]["ids"][0].as_str().unwrap_or_default().to_string();
                let decoy = serde_json::json!({"id": "ef".repeat(32), "kind": 1059});
                let other_sub = serde_json::json!(["EVENT", "other", served]);
                for frame in [
                    other_sub,
                    serde_json::json!(["EVENT", sub, decoy]),
                    serde_json::json!(["NOTICE", "hello"]),
                ] {
                    ws.send(Message::Text(frame.to_string())).await.unwrap();
                }
                if asked == served["id"] {
                    let frame = serde_json::json!(["EVENT", sub, served]);
                    ws.send(Message::Text(frame.to_string())).await.unwrap();
                } else {
                    let frame = serde_json::json!(["EOSE", sub]);
                    ws.send(Message::Text(frame.to_string())).await.unwrap();
                }
                while let Some(Ok(_)) = ws.next().await {}
            }
        });
    });
    let addr = addr_rx.recv().unwrap();
    let url = format!("ws://{addr}");
    let got = block_on(fetch_event(&wanted, &url)).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&got).unwrap(),
        event
    );
    assert!(block_on(fetch_event(&"01".repeat(32), &url)).is_err());
}
