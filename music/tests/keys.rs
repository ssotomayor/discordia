#![cfg(feature = "audio-keys")]

use discordia_bot::{
    BotIdentity,
    protocol::{ActivityInstance, ActivityParticipant, ActivityRole, Id, activity_audio_epoch},
};
use discordia_music::activity::keys::PublicationKeys;

fn instance(app: &BotIdentity, listener: &BotIdentity) -> ActivityInstance {
    ActivityInstance {
        id: Id::new_v4(),
        channel_id: Id::new_v4(),
        app: app.pubkey().into(),
        max_players: 16,
        participants: vec![ActivityParticipant {
            pubkey: listener.pubkey().into(),
            role: ActivityRole::Player,
        }],
    }
}

#[test]
fn unchanged_snapshots_preserve_a_publication_but_membership_aba_rotates_it() {
    let app = BotIdentity::generate();
    let alice = BotIdentity::generate();
    let bob = BotIdentity::generate();
    let original = instance(&app, &alice);
    let mut keys = PublicationKeys::new(original.clone()).unwrap();
    let request = Id::new_v4();
    let epoch = activity_audio_epoch(&original);
    let (first, encrypted) = keys.seal(&app, alice.pubkey(), request, &epoch).unwrap();
    let old_key = alice
        .open_activity_audio_key(app.pubkey(), &first, &encrypted)
        .unwrap();
    assert!(!keys.sync(original.clone()).unwrap());
    assert_eq!(
        keys.seal(&app, alice.pubkey(), request, &epoch).unwrap().0,
        first
    );

    let mut joined = original.clone();
    joined.participants.push(ActivityParticipant {
        pubkey: bob.pubkey().into(),
        role: ActivityRole::Spectator,
    });
    assert!(keys.sync(joined.clone()).unwrap());
    assert!(keys.seal(&app, alice.pubkey(), request, &epoch).is_err());
    let (scope, encrypted) = keys
        .seal(&app, bob.pubkey(), request, &activity_audio_epoch(&joined))
        .unwrap();
    assert!(
        bob.open_activity_audio_key(app.pubkey(), &scope, &encrypted)
            .is_ok()
    );
    assert!(keys.sync(original).unwrap());
    assert!(keys.seal(&app, bob.pubkey(), request, &epoch).is_err());
    let (last, encrypted) = keys.seal(&app, alice.pubkey(), request, &epoch).unwrap();
    let new_key = alice
        .open_activity_audio_key(app.pubkey(), &last, &encrypted)
        .unwrap();
    assert_ne!(first.publication, last.publication);
    assert_ne!(old_key.as_bytes(), new_key.as_bytes());
    assert!(
        alice
            .open_activity_audio_key(app.pubkey(), &first, &encrypted)
            .is_err()
    );
}

#[test]
fn unauthorized_or_retired_sessions_cannot_disclose_keys() {
    let app = BotIdentity::generate();
    let alice = BotIdentity::generate();
    let outsider = BotIdentity::generate();
    let original = instance(&app, &alice);
    let mut keys = PublicationKeys::new(original.clone()).unwrap();
    let epoch = activity_audio_epoch(&original);
    let request = Id::new_v4();
    assert!(keys.seal(&app, outsider.pubkey(), request, &epoch).is_err());
    assert!(
        keys.seal(&outsider, alice.pubkey(), request, &epoch)
            .is_err()
    );
    let mut ended = original.clone();
    ended.participants.clear();
    assert!(keys.sync(ended).is_err());
    assert!(keys.seal(&app, alice.pubkey(), request, &epoch).is_err());
    assert!(keys.sync(original.clone()).unwrap());
    keys.retire();
    assert!(keys.seal(&app, alice.pubkey(), request, &epoch).is_err());
    let mut wrong_channel = original;
    wrong_channel.channel_id = Id::new_v4();
    assert!(keys.sync(wrong_channel).is_err());
    assert!(keys.seal(&app, alice.pubkey(), request, &epoch).is_err());
}

#[test]
fn roles_and_service_restarts_rotate_publication_keys() {
    let app = BotIdentity::generate();
    let alice = BotIdentity::generate();
    let mut current = instance(&app, &alice);
    let mut keys = PublicationKeys::new(current.clone()).unwrap();
    let request = Id::new_v4();
    let first = keys
        .seal(
            &app,
            alice.pubkey(),
            request,
            &activity_audio_epoch(&current),
        )
        .unwrap()
        .0;
    current.participants[0].role = ActivityRole::Spectator;
    assert!(keys.sync(current.clone()).unwrap());
    let (changed, ciphertext) = keys
        .seal(
            &app,
            alice.pubkey(),
            request,
            &activity_audio_epoch(&current),
        )
        .unwrap();
    assert_ne!(first.publication, changed.publication);
    let old_key = alice
        .open_activity_audio_key(app.pubkey(), &changed, &ciphertext)
        .unwrap();
    let restarted = PublicationKeys::new(current.clone()).unwrap();
    let (last, ciphertext) = restarted
        .seal(
            &app,
            alice.pubkey(),
            request,
            &activity_audio_epoch(&current),
        )
        .unwrap();
    let new_key = alice
        .open_activity_audio_key(app.pubkey(), &last, &ciphertext)
        .unwrap();
    assert_ne!(changed.publication, last.publication);
    assert_ne!(old_key.as_bytes(), new_key.as_bytes());
}
