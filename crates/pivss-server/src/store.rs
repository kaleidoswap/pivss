//! Versioned key-value storage behind the server.
//!
//! Two implementations of the same VSS semantics:
//! - [`MemoryStore`]: in-process, for demo mode and tests.
//! - [`VssHttpStore`]: protobuf-over-HTTP client for a real LDK
//!   [vss-server](https://github.com/lightningdevkit/vss-server) instance.
//!
//! VSS keeps only the latest value per key (its `version` is an optimistic-
//! concurrency counter, not history). PIVSS therefore stores each backup
//! version under its own key (`backups/<id>/v<n>`), which makes history
//! retention explicit and listable via `listKeyVersions` with a prefix.

use async_trait::async_trait;
use pivss_core::proto::*;
use prost::Message;
use secp256k1::{PublicKey, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 64-byte constant signed to prove private-key knowledge to a vss-server
/// running the default signature authorizer. Must match, byte for byte,
/// `SIGNING_CONSTANT` in lightningdevkit/vss-server `auth-impls/src/signature.rs`.
const VSS_SIGNING_CONSTANT: &[u8] =
    b"VSS Signature Authorizer Signing Salt Constant..................";

/// Build the `Authorization` header value the vss-server's
/// `SignatureValidatingAuthorizer` expects:
///   hex(pubkey_compressed_33) ‖ hex(ecdsa_sig_compact_64) ‖ unix_secs
/// where the signature is over SHA256(CONSTANT ‖ pubkey ‖ ascii(unix_secs)).
fn vss_auth_header(signing_key: &SecretKey) -> String {
    let secp = Secp256k1::signing_only();
    let pubkey = PublicKey::from_secret_key(&secp, signing_key);
    let pubkey_bytes = pubkey.serialize(); // 33-byte compressed
    let time_str = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();

    let mut h = Sha256::new();
    h.update(VSS_SIGNING_CONSTANT);
    h.update(pubkey_bytes);
    h.update(time_str.as_bytes());
    let digest: [u8; 32] = h.finalize().into();
    let sig = secp.sign_ecdsa(&secp256k1::Message::from_digest(digest), signing_key);

    format!(
        "{}{}{}",
        hex::encode(pubkey_bytes),
        hex::encode(sig.serialize_compact()),
        time_str
    )
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("version conflict on key {0}")]
    Conflict(String),
    #[error("no such key: {0}")]
    NoSuchKey(String),
    #[error("storage backend error: {0}")]
    Backend(String),
}

#[async_trait]
pub trait VersionedStore: Send + Sync {
    /// Get the latest value+version for a key.
    async fn get(&self, key: &str) -> Result<Option<KeyValue>, StoreError>;

    /// Conditional write with VSS semantics: `version` must equal the current
    /// server-side version (0 for first write, -1 to skip the check).
    /// Returns the value's new version.
    async fn put(&self, key: &str, version: i64, value: Vec<u8>) -> Result<i64, StoreError>;

    /// List keys (and their versions) under a prefix. Values are not returned.
    async fn list(&self, prefix: &str) -> Result<Vec<KeyValue>, StoreError>;

    fn backend_name(&self) -> &'static str;
}

// ---------------------------------------------------------------------------
// In-memory store (demo/tests)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct MemoryStore {
    map: Mutex<BTreeMap<String, KeyValue>>,
}

#[async_trait]
impl VersionedStore for MemoryStore {
    async fn get(&self, key: &str) -> Result<Option<KeyValue>, StoreError> {
        Ok(self.map.lock().unwrap().get(key).cloned())
    }

    async fn put(&self, key: &str, version: i64, value: Vec<u8>) -> Result<i64, StoreError> {
        let mut map = self.map.lock().unwrap();
        let current = map.get(key).map(|kv| kv.version);
        let new_version = match (version, current) {
            (-1, _) => 1,
            (0, None) => 1,
            (v, Some(cur)) if v == cur => cur + 1,
            _ => return Err(StoreError::Conflict(key.to_string())),
        };
        map.insert(
            key.to_string(),
            KeyValue {
                key: key.to_string(),
                version: new_version,
                value,
            },
        );
        Ok(new_version)
    }

    async fn list(&self, prefix: &str) -> Result<Vec<KeyValue>, StoreError> {
        Ok(self
            .map
            .lock()
            .unwrap()
            .range(prefix.to_string()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(_, kv)| KeyValue {
                key: kv.key.clone(),
                version: kv.version,
                value: vec![],
            })
            .collect())
    }

    fn backend_name(&self) -> &'static str {
        "memory"
    }
}

// ---------------------------------------------------------------------------
// VSS HTTP store (LDK vss-server)
// ---------------------------------------------------------------------------

pub struct VssHttpStore {
    base_url: String,
    store_id: String,
    /// secp256k1 key signing every request for a sig-auth vss-server.
    signing_key: SecretKey,
    http: reqwest::Client,
}

impl VssHttpStore {
    pub fn new(
        base_url: impl Into<String>,
        store_id: impl Into<String>,
        signing_key: SecretKey,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            store_id: store_id.into(),
            signing_key,
            http: reqwest::Client::new(),
        }
    }

    async fn call<Req: Message, Resp: Message + Default>(
        &self,
        endpoint: &str,
        req: &Req,
    ) -> Result<Resp, StoreError> {
        let url = format!("{}/{}", self.base_url, endpoint);
        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/octet-stream")
            .header("Authorization", vss_auth_header(&self.signing_key))
            .body(req.encode_to_vec())
            .send()
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?;

        let status = resp.status();
        let body = resp
            .bytes()
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?;

        if status.is_success() {
            Resp::decode(body.as_ref()).map_err(|e| StoreError::Backend(e.to_string()))
        } else {
            match ErrorResponse::decode(body.as_ref()) {
                Ok(err) if err.error_code == ErrorCode::ConflictException as i32 => {
                    Err(StoreError::Conflict(err.message))
                }
                Ok(err) if err.error_code == ErrorCode::NoSuchKeyException as i32 => {
                    Err(StoreError::NoSuchKey(err.message))
                }
                Ok(err) => Err(StoreError::Backend(format!(
                    "vss error {}: {}",
                    err.error_code, err.message
                ))),
                Err(_) => Err(StoreError::Backend(format!("vss http {status}"))),
            }
        }
    }
}

#[async_trait]
impl VersionedStore for VssHttpStore {
    async fn get(&self, key: &str) -> Result<Option<KeyValue>, StoreError> {
        let req = GetObjectRequest {
            store_id: self.store_id.clone(),
            key: key.to_string(),
        };
        match self.call::<_, GetObjectResponse>("getObject", &req).await {
            Ok(resp) => Ok(resp.value),
            Err(StoreError::NoSuchKey(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn put(&self, key: &str, version: i64, value: Vec<u8>) -> Result<i64, StoreError> {
        let req = PutObjectRequest {
            store_id: self.store_id.clone(),
            global_version: None,
            transaction_items: vec![KeyValue {
                key: key.to_string(),
                version,
                value,
            }],
            delete_items: vec![],
        };
        // Endpoint is plural: vss-server routes writes at `/putObjects`.
        self.call::<_, PutObjectResponse>("putObjects", &req)
            .await?;
        // VSS increments server-side; mirror the client-side bookkeeping rule.
        Ok(if version == -1 { 1 } else { version + 1 })
    }

    async fn list(&self, prefix: &str) -> Result<Vec<KeyValue>, StoreError> {
        let mut out = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let req = ListKeyVersionsRequest {
                store_id: self.store_id.clone(),
                key_prefix: Some(prefix.to_string()),
                page_size: Some(500),
                page_token: page_token.clone(),
            };
            let resp: ListKeyVersionsResponse = self.call("listKeyVersions", &req).await?;
            out.extend(resp.key_versions);
            match resp.next_page_token {
                Some(t) if !t.is_empty() => page_token = Some(t),
                _ => break,
            }
        }
        Ok(out)
    }

    fn backend_name(&self) -> &'static str {
        "vss"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_store_versioning_semantics() {
        let s = MemoryStore::default();
        // first write must use version 0
        assert!(matches!(
            s.put("k", 5, b"x".to_vec()).await,
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(s.put("k", 0, b"v1".to_vec()).await.unwrap(), 1);
        // conditional write with stale version conflicts
        assert!(matches!(
            s.put("k", 0, b"v2".to_vec()).await,
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(s.put("k", 1, b"v2".to_vec()).await.unwrap(), 2);
        // non-conditional write resets to 1
        assert_eq!(s.put("k", -1, b"v3".to_vec()).await.unwrap(), 1);
        let kv = s.get("k").await.unwrap().unwrap();
        assert_eq!(kv.value, b"v3");

        s.put("prefix/a", 0, b"1".to_vec()).await.unwrap();
        s.put("prefix/b", 0, b"2".to_vec()).await.unwrap();
        let listed = s.list("prefix/").await.unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|kv| kv.value.is_empty()));
    }

    /// Independently verify our `Authorization` header exactly the way
    /// lightningdevkit/vss-server's `SignatureValidatingAuthorizer` does, so a
    /// regression in the wire format is caught without a live server. This
    /// mirrors `auth-impls/src/signature.rs::verify` byte for byte.
    fn server_side_verify(header: &str) -> Result<String, &'static str> {
        if header.len() <= (33 + 64) * 2 || !header.is_ascii() {
            return Err("bad length/chars");
        }
        let pubkey_hex = &header[..33 * 2];
        let sig_hex = &header[33 * 2..(33 + 64) * 2];
        let time_str = &header[(33 + 64) * 2..];

        let pubkey_bytes = hex::decode(pubkey_hex).map_err(|_| "pubkey not hex")?;
        let sig_bytes = hex::decode(sig_hex).map_err(|_| "sig not hex")?;
        let pubkey = PublicKey::from_slice(&pubkey_bytes).map_err(|_| "bad pubkey")?;
        let sig = secp256k1::ecdsa::Signature::from_compact(&sig_bytes).map_err(|_| "bad sig")?;

        let mut h = Sha256::new();
        h.update(VSS_SIGNING_CONSTANT);
        h.update(&pubkey_bytes);
        h.update(time_str.as_bytes());
        let digest: [u8; 32] = h.finalize().into();
        let msg = secp256k1::Message::from_digest(digest);

        Secp256k1::verification_only()
            .verify_ecdsa(&msg, &sig, &pubkey)
            .map_err(|_| "signature invalid")?;
        Ok(pubkey_hex.to_string())
    }

    #[test]
    fn vss_auth_header_verifies_like_the_server() {
        let sk = SecretKey::from_slice(&[42u8; 32]).unwrap();
        let header = vss_auth_header(&sk);

        // The server accepts it, and attributes it to our pubkey.
        let expected_pk =
            hex::encode(PublicKey::from_secret_key(&Secp256k1::new(), &sk).serialize());
        assert_eq!(server_side_verify(&header).unwrap(), expected_pk);

        // A tampered signature byte is rejected.
        let mut bad = header.clone().into_bytes();
        let i = 33 * 2 + 5; // inside the signature region
        bad[i] = if bad[i] == b'0' { b'1' } else { b'0' };
        assert!(server_side_verify(&String::from_utf8(bad).unwrap()).is_err());

        // A different key produces a header attributed to that other key.
        let sk2 = SecretKey::from_slice(&[7u8; 32]).unwrap();
        let pk2 = hex::encode(PublicKey::from_secret_key(&Secp256k1::new(), &sk2).serialize());
        assert_eq!(server_side_verify(&vss_auth_header(&sk2)).unwrap(), pk2);
    }
}
