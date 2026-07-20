//! Real BOLT12 payments via a `pivss_ln::BreezWallet`. Disabled by default
//! (demo mode uses the static `bolt12_offer` config string instead); once
//! enabled, the server only ever trusts payments its own wallet observed.

use crate::config::LightningConfig;
use pivss_ln::{BreezWallet, IncomingPayment, LiquidNetwork};
use std::path::Path;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

pub struct LnState {
    pub wallet: Arc<BreezWallet>,
    /// This provider's durable BOLT12 offer, advertised in the announcement.
    /// Behind a lock so `/api/v1/bolt12/refresh` can re-fetch it at runtime.
    pub offer: RwLock<String>,
}

impl LnState {
    pub fn offer(&self) -> String {
        self.offer.read().unwrap().clone()
    }

    /// Ask the wallet for its offer again. BOLT12 offers from breez-sdk-liquid
    /// are durable by design — this will very likely return the *same* offer
    /// string unless the wallet's own state changed, but it's the only "give
    /// me a fresh read" primitive the SDK exposes, so that's what this calls.
    pub async fn refresh_offer(&self, description: &str) -> anyhow::Result<(String, bool)> {
        let new_offer = self.wallet.receive_offer(description).await?;
        let mut slot = self.offer.write().unwrap();
        let changed = *slot != new_offer;
        *slot = new_offer.clone();
        Ok((new_offer, changed))
    }
}

fn parse_network(s: &str) -> anyhow::Result<LiquidNetwork> {
    match s.to_ascii_lowercase().as_str() {
        "regtest" => Ok(LiquidNetwork::Regtest),
        "mainnet" => Ok(LiquidNetwork::Mainnet),
        other => anyhow::bail!(
            "unsupported lightning.network '{other}' (use \"regtest\" or \"mainnet\" — \
             testnet is not supported by breez-sdk-liquid)"
        ),
    }
}

/// Connect the provider's real wallet and create its durable receive offer.
/// Returns the state plus a channel of confirmed incoming payments the
/// caller is responsible for draining (see `AppState`'s payment-matching loop).
pub async fn connect(
    config: &LightningConfig,
    data_dir: &Path,
    description: &str,
) -> anyhow::Result<(LnState, mpsc::UnboundedReceiver<IncomingPayment>)> {
    let network = parse_network(&config.network)?;

    // config.toml wins; BREEZ_API_KEY (including via a .env file) is the
    // fallback, so a key never has to be committed to config.toml.
    let api_key = if !config.api_key.is_empty() {
        Some(config.api_key.clone())
    } else {
        std::env::var("BREEZ_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
    };

    if matches!(network, LiquidNetwork::Mainnet) && api_key.is_none() {
        anyhow::bail!(
            "lightning.network = \"mainnet\" requires an API key — set lightning.api_key \
             in config.toml or BREEZ_API_KEY in the environment/.env \
             (get a free key at https://breez.technology)"
        );
    }

    let mnemonic_path = data_dir.join("breez-mnemonic.txt");
    let mnemonic = if !config.mnemonic.is_empty() {
        config.mnemonic.clone()
    } else if mnemonic_path.exists() {
        std::fs::read_to_string(&mnemonic_path)?.trim().to_string()
    } else {
        let m = pivss_ln::generate_mnemonic()?;
        std::fs::write(&mnemonic_path, &m)?;
        m
    };

    let working_dir = data_dir.join("breez-wallet");
    let (wallet, rx) = BreezWallet::connect(network, api_key, &working_dir, &mnemonic).await?;
    let wallet = Arc::new(wallet);

    let offer = wallet.receive_offer(description).await?;

    Ok((
        LnState {
            wallet,
            offer: RwLock::new(offer),
        },
        rx,
    ))
}
