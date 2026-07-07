//! Application state and the backup service logic (storage layout, torrent
//! creation, proofs, payments).

use crate::announce::RelayResult;
use crate::config::Config;
use crate::seeder::{SeedStatus, Seeder};
use crate::store::{StoreError, VersionedStore};
use pivss_core::manifest::*;
use pivss_core::nostr::{Event, NostrKeys};
use pivss_core::proof::{compute_proof, StorageChallenge, StorageProof};
use pivss_core::torrent::create_torrent;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub struct AppState {
    pub config: Config,
    pub store: Arc<dyn VersionedStore>,
    pub seeder: Arc<dyn Seeder>,
    pub keys: NostrKeys,
    pub started_at: u64,
    pub last_announcement: Mutex<Option<(Event, Vec<RelayResult>)>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("backup not found: {0}")]
    NotFound(String),
    #[error("backup too large: {0} bytes (max {1})")]
    TooLarge(u64, u64),
    #[error("{0}")]
    Internal(String),
}

fn meta_key(id: &str) -> String {
    format!("backups/{id}/meta")
}

fn data_key(id: &str, version: u64) -> String {
    format!("backups/{id}/v{version}")
}

/// Stable backup id: same filename+kind ⇒ same series ⇒ new upload = new version.
pub fn backup_id(filename: &str, kind: BackupKind) -> String {
    let digest = Sha256::digest(format!("{kind}|{filename}").as_bytes());
    hex::encode(&digest[..8])
}

impl AppState {
    pub fn announcement(&self) -> ServiceAnnouncement {
        ServiceAnnouncement {
            name: self.config.name.clone(),
            description: self.config.description.clone(),
            endpoint: self.config.public_endpoint.clone(),
            bolt12_offer: self.config.bolt12_offer.clone(),
            price_sats_per_mib: self.config.price_sats_per_mib,
            billing_period_secs: self.config.billing_period_secs,
            max_backup_bytes: self.config.max_backup_bytes,
            kinds: vec![BackupKind::Lightning, BackupKind::Rgb, BackupKind::Other],
            pivss_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    async fn get_manifest(&self, id: &str) -> Result<Option<(BackupManifest, i64)>, ServiceError> {
        match self.store.get(&meta_key(id)).await? {
            Some(kv) => {
                let manifest: BackupManifest = serde_json::from_slice(&kv.value)
                    .map_err(|e| ServiceError::Internal(format!("corrupt manifest: {e}")))?;
                Ok(Some((manifest, kv.version)))
            }
            None => Ok(None),
        }
    }

    /// Store a new version of a backup: persist bytes to the versioned store,
    /// materialize a seed file + .torrent, kick the seeder, update the manifest.
    pub async fn store_backup(
        &self,
        filename: &str,
        kind: BackupKind,
        label: &str,
        data: Vec<u8>,
    ) -> Result<(BackupManifest, BackupVersion, SeedStatus, u64), ServiceError> {
        if data.len() as u64 > self.config.max_backup_bytes {
            return Err(ServiceError::TooLarge(
                data.len() as u64,
                self.config.max_backup_bytes,
            ));
        }

        let id = backup_id(filename, kind);
        let now = now_secs();
        let (mut manifest, meta_version) = match self.get_manifest(&id).await? {
            Some((m, v)) => (m, v),
            None => (
                BackupManifest {
                    backup_id: id.clone(),
                    label: label.to_string(),
                    filename: filename.to_string(),
                    kind,
                    latest_version: 0,
                    versions: vec![],
                    created_at: now,
                    updated_at: now,
                },
                0,
            ),
        };

        let version = manifest.latest_version + 1;
        let sha256 = hex::encode(Sha256::digest(&data));

        // 1. Persist the bytes (each version is its own key → first write, version 0).
        self.store
            .put(&data_key(&id, version), 0, data.clone())
            .await?;

        // 2. Materialize the seed dir + torrent.
        let seed_name = format!("{id}-v{version}-{filename}");
        let seed_dir = self
            .config
            .data_dir
            .join("seeds")
            .join(format!("{id}-v{version}"));
        std::fs::create_dir_all(&seed_dir)
            .map_err(|e| ServiceError::Internal(format!("seed dir: {e}")))?;
        std::fs::write(seed_dir.join(&seed_name), &data)
            .map_err(|e| ServiceError::Internal(format!("seed file: {e}")))?;

        let torrent = create_torrent(&seed_name, &data, &self.config.torrent.trackers);
        let torrent_path = seed_dir.join(format!("{seed_name}.torrent"));
        std::fs::write(&torrent_path, &torrent.metainfo)
            .map_err(|e| ServiceError::Internal(format!("torrent file: {e}")))?;

        // 3. Seed it (carl subprocess, or no-op).
        let seed_status = self
            .seeder
            .seed(&torrent.infohash, &torrent_path, &seed_dir)
            .await;

        // 4. Update the manifest (conditional write on its current version).
        let backup_version = BackupVersion {
            version,
            size: data.len() as u64,
            sha256,
            created_at: now,
            infohash: torrent.infohash.clone(),
            magnet: torrent.magnet.clone(),
        };
        manifest.latest_version = version;
        manifest.updated_at = now;
        if !label.is_empty() {
            manifest.label = label.to_string();
        }
        manifest.versions.push(backup_version.clone());

        let meta_bytes =
            serde_json::to_vec(&manifest).map_err(|e| ServiceError::Internal(e.to_string()))?;
        self.store
            .put(&meta_key(&id), meta_version, meta_bytes)
            .await?;

        let quote = quote_msat(data.len() as u64, self.config.price_sats_per_mib);
        Ok((manifest, backup_version, seed_status, quote))
    }

    pub async fn list_backups(&self) -> Result<Vec<BackupManifest>, ServiceError> {
        let keys = self.store.list("backups/").await?;
        let mut out = Vec::new();
        for kv in keys.iter().filter(|kv| kv.key.ends_with("/meta")) {
            if let Some(found) = self.store.get(&kv.key).await? {
                if let Ok(m) = serde_json::from_slice::<BackupManifest>(&found.value) {
                    out.push(m);
                }
            }
        }
        out.sort_by_key(|m| std::cmp::Reverse(m.updated_at));
        Ok(out)
    }

    pub async fn get_backup(&self, id: &str) -> Result<BackupManifest, ServiceError> {
        self.get_manifest(id)
            .await?
            .map(|(m, _)| m)
            .ok_or_else(|| ServiceError::NotFound(id.to_string()))
    }

    pub async fn get_backup_data(&self, id: &str, version: u64) -> Result<Vec<u8>, ServiceError> {
        self.store
            .get(&data_key(id, version))
            .await?
            .map(|kv| kv.value)
            .ok_or_else(|| ServiceError::NotFound(format!("{id} v{version}")))
    }

    /// Answer a proof-of-storage challenge by reading the bytes back from the
    /// versioned store (the source of truth) — not from any cache.
    pub async fn answer_challenge(
        &self,
        id: &str,
        version: Option<u64>,
        challenge: &StorageChallenge,
    ) -> Result<(StorageProof, u64), ServiceError> {
        let manifest = self.get_backup(id).await?;
        let version = version.unwrap_or(manifest.latest_version);
        let data = self.get_backup_data(id, version).await?;
        Ok((compute_proof(challenge, &data), version))
    }

    pub async fn record_payment(
        &self,
        backup_id: &str,
        amount_msat: u64,
        method: &str,
        note: Option<String>,
    ) -> Result<PaymentRecord, ServiceError> {
        // Ensure the backup exists.
        self.get_backup(backup_id).await?;
        let payment_id = hex::encode(rand::random::<[u8; 8]>());
        let record = PaymentRecord {
            payment_id: payment_id.clone(),
            backup_id: backup_id.to_string(),
            amount_msat,
            paid_at: now_secs(),
            method: method.to_string(),
            note,
        };
        let bytes =
            serde_json::to_vec(&record).map_err(|e| ServiceError::Internal(e.to_string()))?;
        self.store
            .put(&format!("payments/{payment_id}"), 0, bytes)
            .await?;
        Ok(record)
    }

    pub async fn list_payments(&self) -> Result<Vec<PaymentRecord>, ServiceError> {
        let keys = self.store.list("payments/").await?;
        let mut out = Vec::new();
        for kv in keys {
            if let Some(found) = self.store.get(&kv.key).await? {
                if let Ok(p) = serde_json::from_slice::<PaymentRecord>(&found.value) {
                    out.push(p);
                }
            }
        }
        out.sort_by_key(|p| std::cmp::Reverse(p.paid_at));
        Ok(out)
    }
}
