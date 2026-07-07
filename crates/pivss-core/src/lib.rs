//! pivss-core — shared types and primitives for the P2P Incentivized
//! Versioned Storage Service:
//!
//! - [`proto`]: hand-written prost messages matching LDK's `vss-server` API
//! - [`crypto`]: client-side AES-256-GCM/Argon2id envelopes (zero-knowledge server)
//! - [`torrent`]: single-file v1 .torrent creation, infohash and magnet links
//! - [`proof`]: nonce-based proof-of-storage challenges
//! - [`nostr`]: minimal NIP-01 event signing/verification for service announcements
//! - [`manifest`]: backup manifests and service-announcement payloads

pub mod crypto;
pub mod manifest;
pub mod nostr;
pub mod proof;
pub mod proto;
pub mod torrent;
