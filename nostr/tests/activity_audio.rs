use discordia_nostr::activity_audio::{AudioKey, KeyContext, open, seal};
use secp256k1::{Secp256k1, SecretKey};

fn identity(value: u8) -> (SecretKey, String) {
    let secret = SecretKey::from_slice(&[value; 32]).unwrap();
    let public = hex::encode(secret.x_only_public_key(&Secp256k1::new()).0.serialize());
    (secret, public)
}

fn context() -> KeyContext {
    KeyContext {
        instance: [1; 16],
        channel: [2; 16],
        epoch: [3; 16],
        request: [4; 16],
        publication: [5; 16],
    }
}

#[test]
fn only_the_intended_listener_opens_the_bound_key() {
    let (publisher, public) = identity(1);
    let (listener, recipient) = identity(2);
    let (outsider, _) = identity(3);
    let key = AudioKey::generate();
    let scope = context();
    let ciphertext = seal(&publisher, &recipient, &scope, &key).unwrap();
    assert!(ciphertext.len() < 2048);
    assert_eq!(
        open(&listener, &public, &scope, &ciphertext)
            .unwrap()
            .as_bytes(),
        key.as_bytes()
    );
    assert!(open(&outsider, &public, &scope, &ciphertext).is_err());
    assert!(open(&publisher, &recipient, &scope, &ciphertext).is_err());
    assert!(!format!("{key:?}").contains(&hex::encode(key.as_bytes())));
    assert_ne!(
        seal(&publisher, &recipient, &scope, &key).unwrap(),
        ciphertext
    );
}

#[test]
fn every_scope_field_and_the_request_nonce_prevent_cross_context_replay() {
    let (publisher, public) = identity(1);
    let (listener, recipient) = identity(2);
    let original = context();
    let ciphertext = seal(&publisher, &recipient, &original, &AudioKey::generate()).unwrap();
    for index in 0..5 {
        let mut changed = original.clone();
        match index {
            0 => changed.instance[0] ^= 1,
            1 => changed.channel[0] ^= 1,
            2 => changed.epoch[0] ^= 1,
            3 => changed.request[0] ^= 1,
            _ => changed.publication[0] ^= 1,
        }
        assert!(open(&listener, &public, &changed, &ciphertext).is_err());
    }
    let mut corrupt = ciphertext.into_bytes();
    corrupt[40] = if corrupt[40] == b'A' { b'B' } else { b'A' };
    assert!(
        open(
            &listener,
            &public,
            &original,
            std::str::from_utf8(&corrupt).unwrap()
        )
        .is_err()
    );
    assert!(open(&listener, &public, &original, &"x".repeat(2049)).is_err());
    assert!(
        seal(
            &publisher,
            "not-a-public-key",
            &original,
            &AudioKey::generate()
        )
        .is_err()
    );
}

#[test]
fn ordinary_encrypted_messages_cannot_be_used_as_audio_keys() {
    use discordia_nostr::nip44;
    let (publisher, public) = identity(1);
    let (listener, recipient) = identity(2);
    let conversation = nip44::conversation_key(&publisher, &recipient).unwrap();
    let payload = serde_json::json!({
        "purpose": "another-feature", "context": context(),
        "publisher": public, "recipient": recipient, "key": vec![7; 32],
    });
    let ciphertext = nip44::encrypt(&conversation, &payload.to_string()).unwrap();
    assert!(open(&listener, &public, &context(), &ciphertext).is_err());
}
