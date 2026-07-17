use clap::{Parser, Subcommand, ValueEnum};
use pivss_client::{ApiClient, Ledger, LedgerEntry, VerifyOutcome};
use pivss_core::manifest::BackupKind;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "pivss-client", about = "Client for a PIVSS backup server")]
struct Args {
    /// PIVSS server base URL.
    #[arg(short, long, default_value = "http://127.0.0.1:8339")]
    server: String,
    /// Where the client keeps its local ledger.
    #[arg(long, default_value = "./pivss-client-state")]
    state_dir: PathBuf,
    /// Optional convenience encryption passphrase (or set PIVSS_PASSPHRASE).
    /// PIVSS assumes backups are ALREADY encrypted and treats every payload as
    /// an opaque blob — the provider is zero-knowledge either way. Set this
    /// only if you want the client to also encrypt the file for you before
    /// upload (AES-256-GCM / Argon2id) instead of bringing your own ciphertext.
    #[arg(long, env = "PIVSS_PASSPHRASE")]
    passphrase: Option<String>,
    /// Pay for real via an embedded breez-sdk-liquid wallet instead of the
    /// server's mock payment endpoint (which a real deployment disables once
    /// it has a wallet of its own — see `lightning.enable` in config.toml).
    #[arg(long)]
    real_payment: bool,
    /// Network for the embedded client wallet: "regtest" (no API key, but
    /// needs a local Breez regtest stack) or "mainnet" (needs --ln-api-key).
    /// Testnet is not supported by breez-sdk-liquid.
    #[arg(long, default_value = "regtest")]
    ln_network: String,
    /// Breez API key, required for --ln-network mainnet. Free key at
    /// https://breez.technology.
    #[arg(long, env = "BREEZ_API_KEY")]
    ln_api_key: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum KindArg {
    Lightning,
    Rgb,
    Other,
}

#[derive(Clone, Copy, ValueEnum)]
enum FundMethodArg {
    /// On-chain BTC, reverse-swapped into Liquid — fundable from any exchange
    /// or wallet that sends plain on-chain Bitcoin.
    Bitcoin,
    /// Direct L-BTC address — no swap, no swap fee, no minimum, but the
    /// sender needs Liquid BTC already.
    Liquid,
}

impl From<FundMethodArg> for pivss_ln::FundingMethod {
    fn from(m: FundMethodArg) -> Self {
        match m {
            FundMethodArg::Bitcoin => pivss_ln::FundingMethod::BitcoinAddress,
            FundMethodArg::Liquid => pivss_ln::FundingMethod::LiquidAddress,
        }
    }
}

impl From<KindArg> for BackupKind {
    fn from(k: KindArg) -> Self {
        match k {
            KindArg::Lightning => BackupKind::Lightning,
            KindArg::Rgb => BackupKind::Rgb,
            KindArg::Other => BackupKind::Other,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Show server info / service announcement.
    Info,
    /// Upload a file as a new backup version.
    Backup {
        file: PathBuf,
        #[arg(long, value_enum, default_value = "other")]
        kind: KindArg,
        #[arg(long, default_value = "")]
        label: String,
    },
    /// List backups on the server.
    List,
    /// Download a backup version (defaults to latest).
    Restore {
        backup_id: String,
        #[arg(long)]
        version: Option<u64>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Challenge the server to prove it still stores the backup.
    Verify {
        backup_id: String,
        #[arg(long)]
        version: Option<u64>,
    },
    /// Pay for storage (mock payment; prints the BOLT12 offer for real wallets).
    Pay {
        backup_id: String,
        #[arg(long)]
        amount_msat: Option<u64>,
    },
    /// Recurring loop: verify proof-of-storage, pay only when it checks out.
    Watch {
        backup_id: String,
        /// Seconds between verify+pay rounds.
        #[arg(long, default_value = "60")]
        interval: u64,
        /// Number of rounds (0 = forever).
        #[arg(long, default_value = "0")]
        rounds: u64,
    },
    /// Get a destination to top up THIS client's own embedded wallet (real
    /// funds — nothing to do with a PIVSS storage payment). Use --ln-network
    /// to pick mainnet vs regtest.
    Receive {
        #[arg(long, value_enum, default_value = "bitcoin")]
        method: FundMethodArg,
        /// Request an exact amount instead of a reusable address.
        #[arg(long)]
        amount_sat: Option<u64>,
    },
    /// Show this client's own embedded wallet balance (real funds).
    Balance,
}

fn print_verify(outcome: &VerifyOutcome) {
    if outcome.ok {
        println!(
            "✔ proof-of-storage OK (v{}, nonce {}…, full proof {}…)",
            outcome.version,
            &outcome.challenge.nonce[..8],
            &outcome.proof.full_proof[..16]
        );
    } else {
        println!(
            "✘ PROOF FAILED for v{} — server could not demonstrate it stores your data",
            outcome.version
        );
    }
}

/// Reproduce the exact bytes we uploaded for `entry` from the local plaintext:
/// ciphertext (re-sealed with the stored salt+nonce) when encrypted, else the
/// raw file. This is what the proof-of-storage challenge is checked against.
fn local_stored_bytes(
    entry: &pivss_client::LedgerEntry,
    passphrase: Option<&str>,
) -> anyhow::Result<Vec<u8>> {
    let plaintext = std::fs::read(&entry.file_path)?;
    if entry.encrypted {
        let pw = passphrase.ok_or_else(|| {
            anyhow::anyhow!("backup is encrypted — provide --passphrase (or PIVSS_PASSPHRASE)")
        })?;
        Ok(pivss_core::crypto::encrypt_with(
            pw,
            &entry.salt_hex,
            &entry.nonce_hex,
            &plaintext,
        )?)
    } else {
        Ok(plaintext)
    }
}

/// Fetch the server's current, live BOLT12 offer (not the ledger's cached
/// copy — a real wallet's offer is authoritative from the server itself).
async fn get_live_offer(client: &ApiClient) -> anyhow::Result<String> {
    let info = client.info().await?;
    info["announcement"]["bolt12_offer"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("server has no BOLT12 offer configured"))
}

/// Connect this client's own embedded wallet (a separate identity from any
/// provider), generating and persisting a mnemonic under `state_dir` on
/// first use — protect that file like any wallet seed.
async fn connect_client_wallet(
    ln_network: &str,
    ln_api_key: Option<String>,
    state_dir: &std::path::Path,
) -> anyhow::Result<pivss_ln::BreezWallet> {
    let network = match ln_network.to_ascii_lowercase().as_str() {
        "regtest" => pivss_ln::LiquidNetwork::Regtest,
        "mainnet" => pivss_ln::LiquidNetwork::Mainnet,
        other => anyhow::bail!(
            "unsupported --ln-network '{other}' (use \"regtest\" or \"mainnet\" — \
             testnet is not supported by breez-sdk-liquid)"
        ),
    };
    if matches!(network, pivss_ln::LiquidNetwork::Mainnet) && ln_api_key.is_none() {
        anyhow::bail!("--ln-network mainnet requires --ln-api-key (or BREEZ_API_KEY env)");
    }

    std::fs::create_dir_all(state_dir)?;
    let mnemonic_path = state_dir.join("breez-mnemonic.txt");
    let mnemonic = if mnemonic_path.exists() {
        std::fs::read_to_string(&mnemonic_path)?.trim().to_string()
    } else {
        let m = pivss_ln::generate_mnemonic()?;
        std::fs::write(&mnemonic_path, &m)?;
        m
    };

    let working_dir = state_dir.join("breez-wallet");
    let (wallet, _incoming) =
        pivss_ln::BreezWallet::connect(network, ln_api_key, &working_dir, &mnemonic).await?;
    Ok(wallet)
}

async fn verify_from_ledger(
    client: &ApiClient,
    ledger: &Ledger,
    passphrase: Option<&str>,
    backup_id: &str,
    version: Option<u64>,
) -> anyhow::Result<VerifyOutcome> {
    let entry = ledger.entries.get(backup_id).ok_or_else(|| {
        anyhow::anyhow!("backup {backup_id} not in local ledger — upload it first")
    })?;
    let stored = local_stored_bytes(entry, passphrase)?;
    client.verify(backup_id, &stored, version).await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env (walking up from the current dir) so BREEZ_API_KEY etc. can
    // be set there instead of the real shell environment. Silently no-op if
    // there's no .env file — clap's `env = "..."` still works either way.
    let _ = dotenvy::dotenv();
    let args = Args::parse();
    let client = ApiClient::new(&args.server);
    let mut ledger = Ledger::load(&args.state_dir)?;
    // Extracted up front: `match args.cmd` below partially moves `args`, so
    // `&args` (whole-struct borrow) can't be taken afterward — individual
    // field reads are still fine, only the two `connect_client_wallet` calls
    // need these.
    let ln_network = args.ln_network.clone();
    let ln_api_key = args.ln_api_key.clone();
    let state_dir = args.state_dir.clone();
    ledger.server = args.server.clone();

    match args.cmd {
        Cmd::Info => {
            let info = client.info().await?;
            println!("{}", serde_json::to_string_pretty(&info)?);
        }
        Cmd::Backup { file, kind, label } => {
            let payload = std::fs::read(&file)?;
            let filename = file
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("backup.bin")
                .to_string();

            // PIVSS treats the payload as an opaque, already-encrypted blob and
            // uploads it as-is (the provider is zero-knowledge). As an optional
            // convenience, a passphrase makes the client encrypt it first.
            let (upload_bytes, encrypted, salt_hex, nonce_hex) = match &args.passphrase {
                Some(pw) => {
                    let (env, salt, nonce) = pivss_core::crypto::encrypt(pw, &payload)?;
                    (env, true, salt, nonce)
                }
                None => (payload, false, String::new(), String::new()),
            };
            let sha256 = pivss_client::sha256_hex(&upload_bytes);

            let resp = client
                .upload(&filename, kind.into(), &label, upload_bytes)
                .await?;
            println!(
                "stored '{}' as backup {} v{} ({} bytes, {})",
                filename,
                resp.manifest.backup_id,
                resp.stored_version.version,
                resp.stored_version.size,
                if encrypted {
                    "client-encrypted"
                } else {
                    "opaque blob (bring-your-own-encryption)"
                }
            );
            println!("  stored sha256: {sha256}");
            println!("  magnet:        {}", resp.stored_version.magnet);
            println!(
                "  price:         {} msat per billing period — offer: {}",
                resp.quote_msat,
                if resp.bolt12_offer.is_empty() {
                    "(none configured)"
                } else {
                    &resp.bolt12_offer
                }
            );
            ledger.entries.insert(
                resp.manifest.backup_id.clone(),
                LedgerEntry {
                    backup_id: resp.manifest.backup_id.clone(),
                    file_path: std::fs::canonicalize(&file)?,
                    filename,
                    kind: kind.into(),
                    sha256,
                    latest_version: resp.stored_version.version,
                    quote_msat: resp.quote_msat,
                    bolt12_offer: resp.bolt12_offer,
                    encrypted,
                    salt_hex,
                    nonce_hex,
                },
            );
            ledger.save(&args.state_dir)?;
        }
        Cmd::List => {
            for m in client.list().await? {
                println!(
                    "{}  {}  kind={} versions={} latest_v{} updated_at={}",
                    m.backup_id,
                    m.filename,
                    m.kind,
                    m.versions.len(),
                    m.latest_version,
                    m.updated_at
                );
            }
        }
        Cmd::Restore {
            backup_id,
            version,
            output,
        } => {
            let manifests = client.list().await?;
            let m = manifests
                .iter()
                .find(|m| m.backup_id == backup_id)
                .ok_or_else(|| anyhow::anyhow!("backup {backup_id} not found on server"))?;
            let v = version.unwrap_or(m.latest_version);
            let data = client.fetch_data(&backup_id, v).await?;
            // Auto-detect and decrypt PIVSS envelopes.
            let (plaintext, note) = if pivss_core::crypto::is_envelope(&data) {
                let pw = args.passphrase.as_deref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "stored data is encrypted — provide --passphrase (or PIVSS_PASSPHRASE)"
                    )
                })?;
                (pivss_core::crypto::decrypt(pw, &data)?, "decrypted")
            } else {
                (data, "plaintext")
            };
            let out = output.unwrap_or_else(|| PathBuf::from(format!("{}-v{v}", m.filename)));
            std::fs::write(&out, &plaintext)?;
            println!(
                "restored {} v{v} → {} ({} bytes, {note})",
                backup_id,
                out.display(),
                plaintext.len()
            );
        }
        Cmd::Verify { backup_id, version } => {
            let outcome = verify_from_ledger(
                &client,
                &ledger,
                args.passphrase.as_deref(),
                &backup_id,
                version,
            )
            .await?;
            print_verify(&outcome);
            if !outcome.ok {
                std::process::exit(1);
            }
        }
        Cmd::Pay {
            backup_id,
            amount_msat,
        } => {
            let entry = ledger.entries.get(&backup_id);
            let amount = amount_msat
                .or(entry.map(|e| e.quote_msat))
                .ok_or_else(|| anyhow::anyhow!("no quote known — pass --amount-msat"))?;

            if args.real_payment {
                let offer = get_live_offer(&client).await?;
                let wallet =
                    connect_client_wallet(&ln_network, ln_api_key.clone(), &state_dir).await?;
                let amount_sat = amount.div_ceil(1000).max(1);
                println!(
                    "paying BOLT12 offer for {amount_sat} sat (payer_note={backup_id})...\n  {offer}"
                );
                let paid = wallet.pay_offer(&offer, amount_sat, &backup_id).await?;
                println!(
                    "paid {} sat (fees {} sat){}",
                    paid.amount_sat,
                    paid.fees_sat,
                    paid.preimage
                        .map(|p| format!(", preimage {p}"))
                        .unwrap_or_default()
                );
            } else {
                if let Some(offer) = entry
                    .map(|e| e.bolt12_offer.clone())
                    .filter(|o| !o.is_empty())
                {
                    println!(
                        "BOLT12 offer to pay for real (pass --real-payment to do so here):\n  {offer}"
                    );
                }
                let rec = client
                    .pay_mock(&backup_id, amount, Some("pivss-client".into()))
                    .await?;
                println!(
                    "recorded {} payment {} — {} msat for backup {}",
                    rec.method, rec.payment_id, rec.amount_msat, rec.backup_id
                );
            }
        }
        Cmd::Watch {
            backup_id,
            interval,
            rounds,
        } => {
            let wallet = if args.real_payment {
                Some(connect_client_wallet(&ln_network, ln_api_key.clone(), &state_dir).await?)
            } else {
                None
            };

            let mut round = 0u64;
            loop {
                round += 1;
                match verify_from_ledger(
                    &client,
                    &ledger,
                    args.passphrase.as_deref(),
                    &backup_id,
                    None,
                )
                .await
                {
                    Ok(outcome) if outcome.ok => {
                        print_verify(&outcome);
                        let amount_msat = ledger
                            .entries
                            .get(&backup_id)
                            .map(|e| e.quote_msat)
                            .unwrap_or(1000);

                        let paid: anyhow::Result<String> = if let Some(wallet) = &wallet {
                            match get_live_offer(&client).await {
                                Ok(offer) => {
                                    let amount_sat = amount_msat.div_ceil(1000).max(1);
                                    wallet.pay_offer(&offer, amount_sat, &backup_id).await.map(
                                        |p| {
                                            format!(
                                                "{} sat (fees {} sat)",
                                                p.amount_sat, p.fees_sat
                                            )
                                        },
                                    )
                                }
                                Err(e) => Err(e),
                            }
                        } else {
                            client
                                .pay_mock(&backup_id, amount_msat, Some(format!("round {round}")))
                                .await
                                .map(|rec| format!("{} msat ({})", rec.amount_msat, rec.payment_id))
                        };

                        match paid {
                            Ok(msg) => println!("  → paid {msg}"),
                            Err(e) => println!("  → payment failed: {e}"),
                        }
                    }
                    Ok(outcome) => {
                        print_verify(&outcome);
                        println!("  → withholding payment");
                    }
                    Err(e) => println!("✘ verify error: {e} — withholding payment"),
                }
                if rounds > 0 && round >= rounds {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(interval)).await;
            }
        }
        Cmd::Receive { method, amount_sat } => {
            let wallet = connect_client_wallet(&ln_network, ln_api_key.clone(), &state_dir).await?;
            let info = wallet.funding_address(method.into(), amount_sat).await?;
            println!(
                "fund this client's wallet ({ln_network}):\n  {}",
                info.destination
            );
            if let Some(min) = info.min_payer_amount_sat {
                println!("  min: {min} sat");
            }
            if let Some(max) = info.max_payer_amount_sat {
                println!("  max: {max} sat");
            }
            println!("  estimated swap fee: {} sat", info.fees_sat);
            println!(
                "\nwallet seed is at {}/breez-mnemonic.txt — this now controls real funds, protect it like any wallet seed.",
                state_dir.display()
            );
        }
        Cmd::Balance => {
            let wallet = connect_client_wallet(&ln_network, ln_api_key.clone(), &state_dir).await?;
            let (confirmed, pending) = wallet.balance().await?;
            println!("confirmed: {confirmed} sat");
            println!("pending incoming: {pending} sat");
        }
    }
    Ok(())
}
