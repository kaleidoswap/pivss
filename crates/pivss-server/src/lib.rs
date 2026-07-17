//! PIVSS server — P2P Incentivized Versioned Storage Service.
//!
//! Stores Lightning/RGB wallet backups in a versioned store (LDK VSS or
//! in-memory), seeds every version over BitTorrent (via carl), advertises the
//! service + BOLT12 offer on nostr, and answers proof-of-storage challenges
//! that gate the client's recurring payments.

pub mod announce;
pub mod api;
pub mod config;
pub mod ln;
pub mod seeder;
pub mod state;
pub mod store;

use config::Config;
use pivss_core::nostr::NostrKeys;
use seeder::{CarlSeeder, NoopSeeder, Seeder};
use state::{now_secs, AppState};
use std::sync::{Arc, Mutex};
use store::{MemoryStore, VersionedStore, VssHttpStore};

/// Wire up state from config: storage backend, seeder, nostr identity.
pub async fn build_state(config: Config) -> anyhow::Result<Arc<AppState>> {
    std::fs::create_dir_all(&config.data_dir)?;

    let store: Arc<dyn VersionedStore> = match config.storage.backend.as_str() {
        "vss" => Arc::new(VssHttpStore::new(
            config.storage.vss_url.clone(),
            config.storage.store_id.clone(),
        )),
        "memory" => Arc::new(MemoryStore::default()),
        other => anyhow::bail!("unknown storage backend: {other} (use \"memory\" or \"vss\")"),
    };

    let seeder: Arc<dyn Seeder> = if config.torrent.enable {
        match CarlSeeder::detect(&config.torrent.carl_bin, config.torrent.port).await {
            Some(carl) => Arc::new(carl),
            None => Arc::new(NoopSeeder::new(format!(
                "carl binary '{}' not found — torrents are still created under {}/seeds; \
                 install carl (github.com/vincenzopalazzo/carl) or seed with any client",
                config.torrent.carl_bin,
                config.data_dir.display()
            ))),
        }
    } else {
        Arc::new(NoopSeeder::new("torrent seeding disabled in config"))
    };

    // Nostr identity: config key > persisted key > freshly generated.
    let key_path = config.data_dir.join("nostr.key");
    let keys = if !config.nostr.secret_key_hex.is_empty() {
        NostrKeys::from_secret_hex(&config.nostr.secret_key_hex)?
    } else if key_path.exists() {
        NostrKeys::from_secret_hex(std::fs::read_to_string(&key_path)?.trim())?
    } else {
        let keys = NostrKeys::generate();
        std::fs::write(&key_path, keys.secret_hex())?;
        keys
    };

    // Real BOLT12 wallet, only when explicitly enabled — the demo path
    // (static bolt12_offer string, mock payment endpoint) needs neither an
    // API key nor a regtest stack.
    let (ln, ln_events) = if config.lightning.enable {
        let (ln_state, rx) =
            ln::connect(&config.lightning, &config.data_dir, &config.description).await?;
        tracing::info!(offer = %ln_state.offer, "connected real BOLT12 wallet");
        (Some(ln_state), Some(rx))
    } else {
        (None, None)
    };

    let state = Arc::new(AppState {
        config,
        store,
        seeder,
        keys,
        started_at: now_secs(),
        last_announcement: Mutex::new(None),
        ln,
    });

    if let Some(mut rx) = ln_events {
        let state = state.clone();
        tokio::spawn(async move {
            while let Some(payment) = rx.recv().await {
                state.match_and_record_incoming_payment(payment).await;
            }
        });
    }

    Ok(state)
}
