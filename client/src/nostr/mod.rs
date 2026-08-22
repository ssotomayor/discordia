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
//!   own test vectors.
//!
//! Still to come: a relay client, NIP-59 gift wrapping (which is what hides the
//! *sender* from a relay, not just the content), and NIP-17 to give the wrapped
//! events the meaning of a conversation.

pub mod event;
pub mod metadata;
pub mod nip02;
pub mod nip17;
pub mod nip44;
pub mod nip59;
pub mod relay;
pub mod service;
