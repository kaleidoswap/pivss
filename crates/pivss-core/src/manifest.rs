//! Shared API types: backup manifests, service info, payments.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackupKind {
    Lightning,
    Rgb,
    Other,
}

impl std::fmt::Display for BackupKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackupKind::Lightning => write!(f, "lightning"),
            BackupKind::Rgb => write!(f, "rgb"),
            BackupKind::Other => write!(f, "other"),
        }
    }
}

/// One stored version of a backup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupVersion {
    pub version: u64,
    pub size: u64,
    pub sha256: String,
    pub created_at: u64,
    /// v1 infohash of the torrent seeding this version.
    pub infohash: String,
    pub magnet: String,
}

/// Manifest for one logical backup (a versioned series).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupManifest {
    pub backup_id: String,
    pub label: String,
    pub filename: String,
    pub kind: BackupKind,
    pub latest_version: u64,
    pub versions: Vec<BackupVersion>,
    pub created_at: u64,
    pub updated_at: u64,
}

/// What a PIVSS server advertises on nostr (event content, JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAnnouncement {
    pub name: String,
    pub description: String,
    /// Base URL of the HTTP API.
    pub endpoint: String,
    /// BOLT12 offer to pay for storage.
    pub bolt12_offer: String,
    /// Price per stored MiB per billing period, in sats.
    pub price_sats_per_mib: u64,
    /// Billing period the price refers to, in seconds.
    pub billing_period_secs: u64,
    /// Max accepted backup size in bytes.
    pub max_backup_bytes: u64,
    /// Supported backup kinds.
    pub kinds: Vec<BackupKind>,
    /// Protocol version.
    pub pivss_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentRecord {
    pub payment_id: String,
    pub backup_id: String,
    pub amount_msat: u64,
    pub paid_at: u64,
    /// "mock" for the demo payer; later: bolt12 payment hash / payer note,
    /// and eventually a BOLT12 payment-proof (rust-lightning #4297).
    pub method: String,
    pub note: Option<String>,
}

/// Price quote for storing a backup of a given size.
pub fn quote_msat(size_bytes: u64, price_sats_per_mib: u64) -> u64 {
    let mib = size_bytes.div_ceil(1024 * 1024).max(1);
    mib * price_sats_per_mib * 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_rounds_up_and_has_floor() {
        assert_eq!(quote_msat(1, 10), 10_000); // <1 MiB → 1 MiB
        assert_eq!(quote_msat(1024 * 1024, 10), 10_000);
        assert_eq!(quote_msat(1024 * 1024 + 1, 10), 20_000);
        assert_eq!(quote_msat(0, 10), 10_000);
    }
}
