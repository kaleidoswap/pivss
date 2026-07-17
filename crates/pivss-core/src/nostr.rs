//! Minimal NIP-01 nostr event creation/signing — enough to publish service
//! announcements without pulling a full nostr SDK.

use secp256k1::{Keypair, Message, Secp256k1, XOnlyPublicKey};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

fn sha256_32(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Kind used for PIVSS service announcements. Addressable/parameterized-
/// replaceable range (30000..40000): relays keep only the latest event per
/// (pubkey, kind, d-tag), which is what a service listing wants.
pub const SERVICE_ANNOUNCEMENT_KIND: u64 = 38831;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u64,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

#[derive(Clone)]
pub struct NostrKeys {
    keypair: Keypair,
}

impl NostrKeys {
    pub fn generate() -> Self {
        let secp = Secp256k1::new();
        Self {
            keypair: Keypair::new(&secp, &mut rand::thread_rng()),
        }
    }

    pub fn from_secret_hex(hex_sk: &str) -> anyhow::Result<Self> {
        let bytes = hex::decode(hex_sk)?;
        let secp = Secp256k1::new();
        Ok(Self {
            keypair: Keypair::from_seckey_slice(&secp, &bytes)?,
        })
    }

    pub fn secret_hex(&self) -> String {
        hex::encode(self.keypair.secret_bytes())
    }

    pub fn public_hex(&self) -> String {
        let (xonly, _) = XOnlyPublicKey::from_keypair(&self.keypair);
        hex::encode(xonly.serialize())
    }

    /// NIP-19 npub encoding of the public key.
    pub fn npub(&self) -> String {
        let (xonly, _) = XOnlyPublicKey::from_keypair(&self.keypair);
        let hrp = bech32::Hrp::parse("npub").expect("static hrp");
        bech32::encode::<bech32::Bech32>(hrp, &xonly.serialize()).expect("bech32 encode")
    }

    /// Build and sign a NIP-01 event.
    pub fn sign_event(
        &self,
        kind: u64,
        content: &str,
        tags: Vec<Vec<String>>,
        created_at: u64,
    ) -> Event {
        let pubkey = self.public_hex();
        let serialized = json!([0, pubkey, created_at, kind, tags, content]);
        let digest = sha256_32(serialized.to_string().as_bytes());
        let id = hex::encode(digest);

        let secp = Secp256k1::new();
        let msg = Message::from_digest(digest);
        let sig = secp.sign_schnorr(&msg, &self.keypair);

        Event {
            id,
            pubkey,
            created_at,
            kind,
            tags,
            content: content.to_string(),
            sig: hex::encode(sig.as_ref()),
        }
    }
}

/// Verify an event's id and schnorr signature (NIP-01).
pub fn verify_event(ev: &Event) -> bool {
    let serialized = json!([0, ev.pubkey, ev.created_at, ev.kind, ev.tags, ev.content]);
    let digest = sha256_32(serialized.to_string().as_bytes());
    if hex::encode(digest) != ev.id {
        return false;
    }
    let (Ok(sig_bytes), Ok(pk_bytes)) = (hex::decode(&ev.sig), hex::decode(&ev.pubkey)) else {
        return false;
    };
    let Ok(sig) = secp256k1::schnorr::Signature::from_slice(&sig_bytes) else {
        return false;
    };
    let Ok(xonly) = XOnlyPublicKey::from_slice(&pk_bytes) else {
        return false;
    };
    let secp = Secp256k1::new();
    let msg = Message::from_digest(digest);
    secp.verify_schnorr(&sig, &msg, &xonly).is_ok()
}

/// The relay wire message for publishing: `["EVENT", {...}]`.
pub fn client_publish_message(ev: &Event) -> String {
    serde_json::to_string(&json!(["EVENT", ev])).expect("serialize event")
}

/// The relay wire message for a NIP-01 subscription request: `["REQ", sub_id, {...}]`.
pub fn client_query_message(sub_id: &str, kinds: &[u64]) -> String {
    serde_json::to_string(&json!(["REQ", sub_id, { "kinds": kinds }])).expect("serialize req")
}

/// NIP-19 npub encoding of a bare hex public key — for other parties'
/// pubkeys, where only the public half is ever known (discovery, display).
pub fn pubkey_hex_to_npub(pubkey_hex: &str) -> anyhow::Result<String> {
    let bytes = hex::decode(pubkey_hex)?;
    let hrp = bech32::Hrp::parse("npub")?;
    Ok(bech32::encode::<bech32::Bech32>(hrp, &bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let keys = NostrKeys::generate();
        let ev = keys.sign_event(
            SERVICE_ANNOUNCEMENT_KIND,
            r#"{"name":"pivss test"}"#,
            vec![vec!["d".into(), "pivss".into()]],
            1_700_000_000,
        );
        assert!(verify_event(&ev));
        assert!(keys.npub().starts_with("npub1"));
    }

    #[test]
    fn tampered_content_fails_verification() {
        let keys = NostrKeys::generate();
        let mut ev = keys.sign_event(1, "hello", vec![], 1_700_000_000);
        ev.content = "evil".into();
        assert!(!verify_event(&ev));
    }

    #[test]
    fn secret_key_roundtrip() {
        let keys = NostrKeys::generate();
        let restored = NostrKeys::from_secret_hex(&keys.secret_hex()).unwrap();
        assert_eq!(keys.public_hex(), restored.public_hex());
    }

    #[test]
    fn pubkey_npub_matches_keypair_npub() {
        let keys = NostrKeys::generate();
        assert_eq!(pubkey_hex_to_npub(&keys.public_hex()).unwrap(), keys.npub());
    }

    #[test]
    fn query_message_shape() {
        let msg = client_query_message("sub1", &[SERVICE_ANNOUNCEMENT_KIND]);
        let parsed: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(parsed[0], "REQ");
        assert_eq!(parsed[1], "sub1");
        assert_eq!(parsed[2]["kinds"][0], SERVICE_ANNOUNCEMENT_KIND);
    }
}
