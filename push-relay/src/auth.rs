//! Signed relay requests: the construction of `rendezvous/src/verify.rs` over
//! `protocol::push::request_payload`.

use std::collections::HashSet;

use axum::http::HeaderMap;
use discordia_protocol::push::{
    PUBKEY_HEADER, SIGNATURE_HEADER, SIGNED_REQUEST_WINDOW_SECS, TIME_HEADER, request_payload,
};
use secp256k1::schnorr::Signature;
use secp256k1::{Message, Secp256k1, XOnlyPublicKey};
use sha2::{Digest, Sha256};

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    MissingHeader(&'static str),
    BadPubkey,
    BadTime,
    Stale,
    BadSignature,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::MissingHeader(h) => write!(f, "missing {h}"),
            AuthError::BadPubkey => f.write_str("the key must be 64 lowercase hex characters"),
            AuthError::BadTime => f.write_str("the time must be unix seconds"),
            AuthError::Stale => f.write_str("the request time is too far from the relay's clock"),
            AuthError::BadSignature => {
                f.write_str("the signature does not verify for this relay, request and body")
            }
        }
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &'static str) -> Result<&'a str, AuthError> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .ok_or(AuthError::MissingHeader(name))
}

/// The signer's key. `body` is the bytes as received, so a proxy or parser
/// that re-serialises cannot make two bodies verify alike.
pub fn verify(
    audiences: &HashSet<String>,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
    now: u64,
) -> Result<String, AuthError> {
    let pubkey = header(headers, PUBKEY_HEADER)?;
    let time = header(headers, TIME_HEADER)?;
    let signature = header(headers, SIGNATURE_HEADER)?;
    if !discordia_protocol::is_canonical_pubkey(pubkey) {
        return Err(AuthError::BadPubkey);
    }
    let time: u64 = time.parse().map_err(|_| AuthError::BadTime)?;
    if time.abs_diff(now) > SIGNED_REQUEST_WINDOW_SECS {
        return Err(AuthError::Stale);
    }
    let xonly = hex::decode(pubkey)
        .ok()
        .and_then(|b| XOnlyPublicKey::from_slice(&b).ok())
        .ok_or(AuthError::BadPubkey)?;
    let sig = hex::decode(signature)
        .ok()
        .and_then(|b| Signature::from_slice(&b).ok())
        .ok_or(AuthError::BadSignature)?;
    let body_hash = hex::encode(Sha256::digest(body));
    let secp = Secp256k1::verification_only();
    // The audience is not sent: it is whichever of our own names the signer used.
    let verified = audiences.iter().any(|audience| {
        request_payload(method, path, audience, time, pubkey, &body_hash).is_some_and(|payload| {
            let digest: [u8; 32] = Sha256::digest(&payload).into();
            secp.verify_schnorr(&sig, &Message::from_digest(digest), &xonly)
                .is_ok()
        })
    });
    if verified {
        Ok(pubkey.to_string())
    } else {
        Err(AuthError::BadSignature)
    }
}

/// Signs as a phone would. The library's tests, the integration tests and any
/// tool driving the relay use it; the relay itself never signs.
pub fn sign_request(
    secret: &secp256k1::SecretKey,
    method: &str,
    path: &str,
    audience: &str,
    time: u64,
    body: &[u8],
) -> Option<(String, String)> {
    let secp = Secp256k1::new();
    let keypair = secp256k1::Keypair::from_secret_key(&secp, secret);
    let pubkey = hex::encode(keypair.x_only_public_key().0.serialize());
    let payload = request_payload(
        method,
        path,
        audience,
        time,
        &pubkey,
        &hex::encode(Sha256::digest(body)),
    )?;
    let digest: [u8; 32] = Sha256::digest(&payload).into();
    let sig = secp.sign_schnorr_no_aux_rand(&Message::from_digest(digest), &keypair);
    Some((pubkey, hex::encode(sig.serialize())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secp256k1::SecretKey;

    const NOW: u64 = 1_700_000_000;

    fn audiences() -> HashSet<String> {
        ["push.example".to_string()].into()
    }

    fn signed(secret: &SecretKey, audience: &str, time: u64, body: &[u8]) -> (HeaderMap, String) {
        let (pubkey, sig) =
            sign_request(secret, "PUT", "/v1/devices", audience, time, body).unwrap();
        let mut h = HeaderMap::new();
        h.insert(PUBKEY_HEADER, pubkey.parse().unwrap());
        h.insert(TIME_HEADER, time.to_string().parse().unwrap());
        h.insert(SIGNATURE_HEADER, sig.parse().unwrap());
        (h, pubkey)
    }

    fn key(seed: u8) -> SecretKey {
        SecretKey::from_slice(&[seed; 32]).unwrap()
    }

    #[test]
    fn a_good_signature_names_its_key() {
        let (h, pubkey) = signed(&key(1), "push.example", NOW, b"{}");
        assert_eq!(
            verify(&audiences(), "PUT", "/v1/devices", &h, b"{}", NOW),
            Ok(pubkey)
        );
    }

    #[test]
    fn everything_the_signature_covers_is_checked() {
        let a = audiences();
        let (h, _) = signed(&key(1), "push.example", NOW, b"{}");
        assert_eq!(
            verify(&a, "PUT", "/v1/devices", &h, b"{ }", NOW),
            Err(AuthError::BadSignature)
        );
        assert_eq!(
            verify(&a, "DELETE", "/v1/devices", &h, b"{}", NOW),
            Err(AuthError::BadSignature)
        );
        assert_eq!(
            verify(&a, "PUT", "/v1/handles", &h, b"{}", NOW),
            Err(AuthError::BadSignature)
        );

        let (other, _) = signed(&key(1), "evil.example", NOW, b"{}");
        assert_eq!(
            verify(&a, "PUT", "/v1/devices", &other, b"{}", NOW),
            Err(AuthError::BadSignature)
        );

        let (mut swapped, _) = signed(&key(1), "push.example", NOW, b"{}");
        let (_, someone_else) = signed(&key(2), "push.example", NOW, b"{}");
        swapped.insert(PUBKEY_HEADER, someone_else.parse().unwrap());
        assert_eq!(
            verify(&a, "PUT", "/v1/devices", &swapped, b"{}", NOW),
            Err(AuthError::BadSignature)
        );
    }

    #[test]
    fn the_time_window_is_three_hundred_seconds_each_way() {
        let a = audiences();
        for t in [NOW - 300, NOW + 300] {
            let (h, _) = signed(&key(1), "push.example", t, b"");
            assert!(verify(&a, "PUT", "/v1/devices", &h, b"", NOW).is_ok());
        }
        for t in [NOW - 301, NOW + 301] {
            let (h, _) = signed(&key(1), "push.example", t, b"");
            assert_eq!(
                verify(&a, "PUT", "/v1/devices", &h, b"", NOW),
                Err(AuthError::Stale)
            );
        }
    }

    #[test]
    fn only_the_canonical_spelling_of_a_key_is_accepted() {
        let (mut h, pubkey) = signed(&key(1), "push.example", NOW, b"");
        h.insert(PUBKEY_HEADER, pubkey.to_ascii_uppercase().parse().unwrap());
        assert_eq!(
            verify(&audiences(), "PUT", "/v1/devices", &h, b"", NOW),
            Err(AuthError::BadPubkey)
        );
        h.remove(PUBKEY_HEADER);
        assert_eq!(
            verify(&audiences(), "PUT", "/v1/devices", &h, b"", NOW),
            Err(AuthError::MissingHeader(PUBKEY_HEADER))
        );
    }
}
