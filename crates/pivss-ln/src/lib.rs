//! Thin wrapper around [breez-sdk-liquid](https://github.com/breez/breez-sdk-liquid),
//! a nodeless Lightning/Liquid wallet, giving PIVSS a real BOLT12 payer/payee:
//! create a durable offer to receive storage payments, pay another party's
//! offer with a payer note used for server-side correlation, and forward
//! confirmed incoming payments as events.
//!
//! Network notes: [`LiquidNetwork::Regtest`] needs no API key and is the
//! default for local/dev use (it does need a local regtest stack — swap
//! proxy, esplora, sync service — see the SDK's own `regtest/` tooling).
//! [`LiquidNetwork::Mainnet`] requires a free Breez API key. `Testnet` is not
//! supported by the underlying SDK as of this version.

use async_trait::async_trait;
use breez_sdk_liquid::prelude::*;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;

pub use breez_sdk_liquid::prelude::LiquidNetwork;

/// A confirmed incoming payment, forwarded from the SDK's event stream. Only
/// `SdkEvent::PaymentSucceeded` events for `Receive`-direction Lightning
/// payments are forwarded — this is the provider's source of truth for
/// "did I actually get paid", replacing any client-asserted payment claim.
#[derive(Debug, Clone)]
pub struct IncomingPayment {
    pub amount_sat: u64,
    pub fees_sat: u64,
    pub payer_note: Option<String>,
    pub payment_hash: Option<String>,
    pub preimage: Option<String>,
}

/// Result of successfully paying an offer.
#[derive(Debug, Clone)]
pub struct PaidPayment {
    pub amount_sat: u64,
    pub fees_sat: u64,
    pub preimage: Option<String>,
}

/// How to fund this wallet from the outside — not a PIVSS storage payment,
/// just topping up the wallet's own spendable balance.
#[derive(Debug, Clone, Copy)]
pub enum FundingMethod {
    /// On-chain BTC, reverse-swapped into Liquid on receipt. Works from any
    /// exchange or wallet that can send plain on-chain Bitcoin.
    BitcoinAddress,
    /// A direct L-BTC address — no swap, no swap fee, no minimum, but the
    /// sender needs Liquid BTC already.
    LiquidAddress,
}

/// A destination to fund this wallet, plus the swap's constraints (if any).
#[derive(Debug, Clone)]
pub struct FundingInfo {
    pub destination: String,
    /// `None` for `LiquidAddress` (no swap involved, hence no floor).
    pub min_payer_amount_sat: Option<u64>,
    pub max_payer_amount_sat: Option<u64>,
    /// Estimated total swap fee for this receive, in sats.
    pub fees_sat: u64,
}

pub struct BreezWallet {
    sdk: Arc<LiquidSdk>,
}

struct ForwardingListener {
    tx: mpsc::UnboundedSender<IncomingPayment>,
}

#[async_trait]
impl EventListener for ForwardingListener {
    async fn on_event(&self, e: SdkEvent) {
        let SdkEvent::PaymentSucceeded { details: payment } = e else {
            return;
        };
        if payment.payment_type != PaymentType::Receive {
            return;
        }
        let PaymentDetails::Lightning {
            payer_note,
            payment_hash,
            preimage,
            ..
        } = payment.details
        else {
            return;
        };
        let _ = self.tx.send(IncomingPayment {
            amount_sat: payment.amount_sat,
            fees_sat: payment.fees_sat,
            payer_note,
            payment_hash,
            preimage,
        });
    }
}

impl BreezWallet {
    /// Connect (or create) a wallet persisted under `working_dir`, from a
    /// BIP39 mnemonic. Returns the wallet plus a channel of confirmed
    /// incoming payments.
    pub async fn connect(
        network: LiquidNetwork,
        breez_api_key: Option<String>,
        working_dir: &Path,
        mnemonic: &str,
    ) -> anyhow::Result<(Self, mpsc::UnboundedReceiver<IncomingPayment>)> {
        std::fs::create_dir_all(working_dir)?;
        let mut config = LiquidSdk::default_config(network, breez_api_key)?;
        config.working_dir = working_dir.to_string_lossy().into_owned();

        let sdk = LiquidSdk::connect(ConnectRequest {
            config,
            mnemonic: Some(mnemonic.to_string()),
            passphrase: None,
            seed: None,
        })
        .await?;

        let (tx, rx) = mpsc::unbounded_channel();
        sdk.add_event_listener(Box::new(ForwardingListener { tx }))
            .await?;

        Ok((Self { sdk }, rx))
    }

    /// Create (or reuse) this wallet's durable BOLT12 offer — the static
    /// payment code advertised in the PIVSS service announcement.
    pub async fn receive_offer(&self, description: &str) -> anyhow::Result<String> {
        let prepare = self
            .sdk
            .prepare_receive_payment(&PrepareReceiveRequest {
                payment_method: PaymentMethod::Bolt12Offer,
                amount: None,
            })
            .await?;
        let res = self
            .sdk
            .receive_payment(&ReceivePaymentRequest {
                prepare_response: prepare,
                description: Some(description.to_string()),
                description_hash: None,
                payer_note: None,
            })
            .await?;
        Ok(res.destination)
    }

    /// Confirmed spendable balance plus any pending incoming amount, in sats.
    pub async fn balance(&self) -> anyhow::Result<(u64, u64)> {
        let info = self.sdk.get_info().await?;
        Ok((
            info.wallet_info.balance_sat,
            info.wallet_info.pending_receive_sat,
        ))
    }

    /// Get a destination to top up this wallet's own spendable balance —
    /// unrelated to PIVSS storage payments, just funding the wallet itself.
    /// Pass `amount_sat` to request an exact amount, or `None` for a
    /// reusable address with no fixed amount.
    pub async fn funding_address(
        &self,
        method: FundingMethod,
        amount_sat: Option<u64>,
    ) -> anyhow::Result<FundingInfo> {
        let payment_method = match method {
            FundingMethod::BitcoinAddress => PaymentMethod::BitcoinAddress,
            FundingMethod::LiquidAddress => PaymentMethod::LiquidAddress,
        };
        let amount = amount_sat.map(|payer_amount_sat| ReceiveAmount::Bitcoin { payer_amount_sat });

        let prepare = self
            .sdk
            .prepare_receive_payment(&PrepareReceiveRequest {
                payment_method,
                amount,
            })
            .await?;
        let min_payer_amount_sat = prepare.min_payer_amount_sat;
        let max_payer_amount_sat = prepare.max_payer_amount_sat;
        let fees_sat = prepare.fees_sat;

        let res = self
            .sdk
            .receive_payment(&ReceivePaymentRequest {
                prepare_response: prepare,
                description: None,
                description_hash: None,
                payer_note: None,
            })
            .await?;

        Ok(FundingInfo {
            destination: res.destination,
            min_payer_amount_sat,
            max_payer_amount_sat,
            fees_sat,
        })
    }

    /// Pay a BOLT12 offer for `amount_sat`, tagging the payment with
    /// `payer_note` so the payee can correlate it (PIVSS uses the backup id).
    pub async fn pay_offer(
        &self,
        offer: &str,
        amount_sat: u64,
        payer_note: &str,
    ) -> anyhow::Result<PaidPayment> {
        let prepare = self
            .sdk
            .prepare_send_payment(&PrepareSendRequest {
                destination: offer.to_string(),
                amount: Some(PayAmount::Bitcoin {
                    receiver_amount_sat: amount_sat,
                }),
                disable_mrh: None,
                payment_timeout_sec: None,
            })
            .await?;
        let res = self
            .sdk
            .send_payment(&SendPaymentRequest {
                prepare_response: prepare,
                use_asset_fees: None,
                payer_note: Some(payer_note.to_string()),
            })
            .await?;
        let preimage = match res.payment.details {
            PaymentDetails::Lightning { preimage, .. } => preimage,
            _ => None,
        };
        Ok(PaidPayment {
            amount_sat: res.payment.amount_sat,
            fees_sat: res.payment.fees_sat,
            preimage,
        })
    }
}

/// Generate a fresh BIP39 (12-word) mnemonic for a new wallet identity.
pub fn generate_mnemonic() -> anyhow::Result<String> {
    let mnemonic = bip39::Mnemonic::generate_in(bip39::Language::English, 12)
        .map_err(|e| anyhow::anyhow!("mnemonic generation failed: {e}"))?;
    Ok(mnemonic.to_string())
}
