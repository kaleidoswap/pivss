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
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum KindArg {
    Lightning,
    Rgb,
    Other,
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
    let args = Args::parse();
    let client = ApiClient::new(&args.server);
    let mut ledger = Ledger::load(&args.state_dir)?;
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
            if let Some(offer) = entry
                .map(|e| e.bolt12_offer.clone())
                .filter(|o| !o.is_empty())
            {
                println!("BOLT12 offer to pay with a real wallet (e.g. breez-sdk):\n  {offer}");
            }
            let rec = client
                .pay_mock(&backup_id, amount, Some("pivss-client".into()))
                .await?;
            println!(
                "recorded {} payment {} — {} msat for backup {}",
                rec.method, rec.payment_id, rec.amount_msat, rec.backup_id
            );
        }
        Cmd::Watch {
            backup_id,
            interval,
            rounds,
        } => {
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
                        let amount = ledger
                            .entries
                            .get(&backup_id)
                            .map(|e| e.quote_msat)
                            .unwrap_or(1000);
                        match client
                            .pay_mock(&backup_id, amount, Some(format!("round {round}")))
                            .await
                        {
                            Ok(rec) => {
                                println!("  → paid {} msat ({})", rec.amount_msat, rec.payment_id)
                            }
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
    }
    Ok(())
}
