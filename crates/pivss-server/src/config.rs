use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub listen: String,
    pub data_dir: PathBuf,
    pub name: String,
    pub description: String,
    /// Public base URL clients should use (goes into the nostr announcement).
    pub public_endpoint: String,
    /// Price per stored MiB per billing period, in sats.
    pub price_sats_per_mib: u64,
    pub billing_period_secs: u64,
    pub max_backup_bytes: u64,
    /// Static BOLT12 offer string used only when `lightning.enable = false`
    /// (pure demo mode, no real wallet). Ignored once a real wallet is
    /// connected — the live wallet's own offer is used instead.
    pub bolt12_offer: String,
    pub storage: StorageConfig,
    pub nostr: NostrConfig,
    pub torrent: TorrentConfig,
    pub lightning: LightningConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8339".into(),
            data_dir: PathBuf::from("./pivss-data"),
            name: "pivss-demo".into(),
            description: "P2P incentivized versioned storage for Lightning & RGB backups".into(),
            public_endpoint: "http://127.0.0.1:8339".into(),
            price_sats_per_mib: 21,
            billing_period_secs: 86_400,
            max_backup_bytes: 32 * 1024 * 1024,
            bolt12_offer: String::new(),
            storage: StorageConfig::default(),
            nostr: NostrConfig::default(),
            torrent: TorrentConfig::default(),
            lightning: LightningConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// "memory" (demo) or "vss" (talks to an LDK vss-server instance).
    pub backend: String,
    pub vss_url: String,
    pub store_id: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: "memory".into(),
            vss_url: "http://127.0.0.1:8080/vss".into(),
            store_id: "pivss".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NostrConfig {
    /// hex secret key; generated and persisted to `<data_dir>/nostr.key` when empty.
    pub secret_key_hex: String,
    pub relays: Vec<String>,
}

impl Default for NostrConfig {
    fn default() -> Self {
        Self {
            secret_key_hex: String::new(),
            // KaleidoSwap's own relay first, plus a public relay for reach.
            relays: vec!["wss://relay.kaleidoswap.com".into(), "wss://nos.lol".into()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TorrentConfig {
    pub enable: bool,
    /// Path to the carl binary (https://github.com/vincenzopalazzo/carl).
    pub carl_bin: String,
    pub port: u16,
    /// Optional trackers embedded in created torrents; carl also supports
    /// DHT and nostr-based discovery without trackers.
    pub trackers: Vec<String>,
}

impl Default for TorrentConfig {
    fn default() -> Self {
        Self {
            enable: true,
            carl_bin: "carl".into(),
            port: 6881,
            trackers: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LightningConfig {
    /// When false (default) the server runs in demo mode with the static
    /// `bolt12_offer` string and no real wallet — no dependency on a
    /// regtest stack or a Breez API key. When true, the server connects a
    /// real breez-sdk-liquid wallet, creates a durable BOLT12 offer, and
    /// only records a payment once the wallet actually observes it —
    /// the mock payment endpoint is disabled while this is on.
    pub enable: bool,
    /// "regtest" (no API key needed, requires a local Breez regtest stack —
    /// see https://github.com/breez/breez-sdk-liquid/tree/main/regtest) or
    /// "mainnet" (requires `api_key`). "testnet" is not supported by the SDK.
    pub network: String,
    /// Required for `network = "mainnet"`. Free key: https://breez.technology
    pub api_key: String,
    /// BIP39 mnemonic (12 words); generated and persisted to
    /// `<data_dir>/breez-mnemonic.txt` when empty. Controls real funds —
    /// protect this file like any wallet seed.
    pub mnemonic: String,
}

impl Default for LightningConfig {
    fn default() -> Self {
        Self {
            enable: false,
            network: "regtest".into(),
            api_key: String::new(),
            mnemonic: String::new(),
        }
    }
}

impl Config {
    pub fn load(path: Option<&PathBuf>) -> anyhow::Result<Self> {
        match path {
            Some(p) => {
                let raw = std::fs::read_to_string(p)?;
                Ok(toml::from_str(&raw)?)
            }
            None => Ok(Self::default()),
        }
    }
}
