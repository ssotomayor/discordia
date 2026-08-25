//! Nostr, as this client speaks it.
//!
//! Identity has always been a Nostr keypair (`crate::identity`), and
//! `crate::blossom` already signs NIP-01 events to upload media — but nothing
//! here has ever talked to a *relay*. This module is where that changes, so
//! direct messages can live on relays a friend can reach from any server
//! instead of in the database of whoever happens to be hosting.
//!
//! Built in slices, because each one is verifiable on its own:
//!
//! - `nip44` — the encryption. Pure, offline, and checked against the spec's
//!   own test vectors, which is what makes a hand-written crypto module a
//!   checked claim rather than an assurance.
//! - `event` — NIP-01 ids and signatures.
//! - `nip59` — the gift wrap. What hides the *sender* from a relay, not just
//!   the content: the outer layer is signed by a throwaway key.
//! - `nip17` — chat semantics over that wrap.
//! - `nip02` — the contact list. Public and replaceable, unlike the messages.
//! - `metadata` — kind 0, the name a key publishes for itself. The only name a
//!   peer you share no server with will ever have.
//! - `relay` — the relay client and pool.
//! - `service` — the task that owns the pool and feeds `AppState`, shaped like
//!   `net::spawn_gateway` deliberately.

pub mod event;
pub mod metadata;
pub mod nip02;
pub mod nip17;
pub mod nip44;
pub mod nip59;
pub mod relay;
pub mod service;
