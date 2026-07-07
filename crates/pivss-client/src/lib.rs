//! PIVSS client library: HTTP API client + local backup ledger.
//!
//! The client keeps a local ledger (`ledger.json` in the state dir) of every
//! backup it uploaded — path, sha256 and latest version — so it can later
//! verify proof-of-storage challenges against its own copy and decide whether
//! to release the recurring BOLT12 payment.

use pivss_core::manifest::{BackupKind, BackupManifest, PaymentRecord};
use pivss_core::proof::{make_challenge, verify_proof, StorageChallenge, StorageProof};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub backup_id: String,
    pub file_path: PathBuf,
    pub filename: String,
    pub kind: BackupKind,
    /// SHA256 of the bytes as stored on the server (ciphertext when encrypted).
    pub sha256: String,
    pub latest_version: u64,
    pub quote_msat: u64,
    pub bolt12_offer: String,
    /// True when the uploaded bytes are an encrypted envelope.
    #[serde(default)]
    pub encrypted: bool,
    /// Salt+nonce (hex) of the latest version, so the client can reproduce the
    /// exact uploaded ciphertext from local plaintext when answering a
    /// proof-of-storage challenge — no second on-disk copy needed.
    #[serde(default)]
    pub salt_hex: String,
    #[serde(default)]
    pub nonce_hex: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Ledger {
    pub server: String,
    pub entries: BTreeMap<String, LedgerEntry>,
}

impl Ledger {
    pub fn load(state_dir: &Path) -> anyhow::Result<Self> {
        let path = state_dir.join("ledger.json");
        if path.exists() {
            Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self, state_dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(state_dir)?;
        std::fs::write(
            state_dir.join("ledger.json"),
            serde_json::to_string_pretty(self)?,
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UploadResponse {
    pub manifest: BackupManifest,
    pub stored_version: pivss_core::manifest::BackupVersion,
    pub quote_msat: u64,
    #[serde(default)]
    pub bolt12_offer: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChallengeResponse {
    pub proof: StorageProof,
    pub version: u64,
}

#[derive(Debug, Clone)]
pub struct VerifyOutcome {
    pub ok: bool,
    pub version: u64,
    pub challenge: StorageChallenge,
    pub proof: StorageProof,
}

pub struct ApiClient {
    base: String,
    http: reqwest::Client,
}

impl ApiClient {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    async fn check(resp: reqwest::Response) -> anyhow::Result<reqwest::Response> {
        if resp.status().is_success() {
            Ok(resp)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("server returned {status}: {body}")
        }
    }

    pub async fn info(&self) -> anyhow::Result<serde_json::Value> {
        let resp = self
            .http
            .get(format!("{}/api/v1/info", self.base))
            .send()
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    pub async fn upload(
        &self,
        filename: &str,
        kind: BackupKind,
        label: &str,
        data: Vec<u8>,
    ) -> anyhow::Result<UploadResponse> {
        let kind_str = kind.to_string();
        let resp = self
            .http
            .post(format!("{}/api/v1/backups", self.base))
            .query(&[
                ("filename", filename),
                ("kind", &kind_str),
                ("label", label),
            ])
            .body(data)
            .send()
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    pub async fn list(&self) -> anyhow::Result<Vec<BackupManifest>> {
        let resp = self
            .http
            .get(format!("{}/api/v1/backups", self.base))
            .send()
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    pub async fn fetch_data(&self, backup_id: &str, version: u64) -> anyhow::Result<Vec<u8>> {
        let resp = self
            .http
            .get(format!(
                "{}/api/v1/backups/{backup_id}/versions/{version}/data",
                self.base
            ))
            .send()
            .await?;
        Ok(Self::check(resp).await?.bytes().await?.to_vec())
    }

    pub async fn challenge(
        &self,
        backup_id: &str,
        challenge: &StorageChallenge,
        version: Option<u64>,
    ) -> anyhow::Result<ChallengeResponse> {
        let resp = self
            .http
            .post(format!(
                "{}/api/v1/backups/{backup_id}/challenge",
                self.base
            ))
            .json(&serde_json::json!({ "challenge": challenge, "version": version }))
            .send()
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    /// Full proof-of-storage round: fresh challenge → server proof → verify
    /// against the local copy.
    pub async fn verify(
        &self,
        backup_id: &str,
        local_data: &[u8],
        version: Option<u64>,
    ) -> anyhow::Result<VerifyOutcome> {
        let challenge = make_challenge(local_data.len() as u64, 3);
        let resp = self.challenge(backup_id, &challenge, version).await?;
        let ok = verify_proof(&challenge, local_data, &resp.proof);
        Ok(VerifyOutcome {
            ok,
            version: resp.version,
            challenge,
            proof: resp.proof,
        })
    }

    /// Record a payment for a backup. Today this uses the server's mock
    /// endpoint; a production client pays the announced BOLT12 offer with a
    /// real wallet (e.g. breez-sdk) and the server matches it on its node.
    pub async fn pay_mock(
        &self,
        backup_id: &str,
        amount_msat: u64,
        note: Option<String>,
    ) -> anyhow::Result<PaymentRecord> {
        let resp = self
            .http
            .post(format!("{}/api/v1/backups/{backup_id}/payments", self.base))
            .json(&serde_json::json!({
                "amount_msat": amount_msat,
                "note": note,
                "method": "mock",
            }))
            .send()
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}
