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
use std::collections::BTreeMap;
use std::sync::Mutex;

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
    http: reqwest::Client,
}

impl VssHttpStore {
    pub fn new(base_url: impl Into<String>, store_id: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            store_id: store_id.into(),
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
        self.call::<_, PutObjectResponse>("putObject", &req).await?;
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
}
