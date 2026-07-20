//! End-to-end test: spin up a real pivss-server (memory store, noop seeder)
//! on a random port and drive it with the client library using the example
//! test backup file — upload, version bump, restore, proof-of-storage,
//! tamper detection, payment.

use pivss_client::ApiClient;
use pivss_core::manifest::BackupKind;
use pivss_core::proof::{make_challenge, verify_proof};
use pivss_server::api::build_router;
use pivss_server::config::Config;

async fn spawn_server() -> (String, tempdir::TempDirGuard) {
    let dir = tempdir::TempDirGuard::new("pivss-e2e");
    let config = Config {
        data_dir: dir.path.clone(),
        bolt12_offer: "lno1qqtestofferpivss".into(),
        torrent: pivss_server::config::TorrentConfig {
            enable: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let state = pivss_server::build_state(config, None).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, build_router(state)).await.unwrap();
    });
    (format!("http://{addr}"), dir)
}

/// Tiny self-cleaning temp dir so test runs don't accumulate state.
mod tempdir {
    use std::path::PathBuf;

    pub struct TempDirGuard {
        pub path: PathBuf,
    }

    impl TempDirGuard {
        pub fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!("{prefix}-{}", hex::encode(rand_bytes())));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    fn rand_bytes() -> [u8; 8] {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as u64
            ^ (std::process::id() as u64) << 32;
        nanos.to_le_bytes()
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

const TEST_BACKUP: &[u8] = include_bytes!("../../../examples/test-backup.json");

#[tokio::test]
async fn full_backup_lifecycle() {
    let (base, _dir) = spawn_server().await;
    let client = ApiClient::new(&base);

    // Service info includes the announcement.
    let info = client.info().await.unwrap();
    assert_eq!(info["announcement"]["bolt12_offer"], "lno1qqtestofferpivss");
    assert!(info["npub"].as_str().unwrap().starts_with("npub1"));

    // Upload v1 of the example test file.
    let up1 = client
        .upload(
            "test-backup.json",
            BackupKind::Lightning,
            "example node state",
            TEST_BACKUP.to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(up1.stored_version.version, 1);
    assert_eq!(up1.manifest.latest_version, 1);
    assert!(up1
        .stored_version
        .magnet
        .starts_with("magnet:?xt=urn:btih:"));
    assert_eq!(
        up1.stored_version.sha256,
        pivss_client::sha256_hex(TEST_BACKUP)
    );
    assert!(up1.quote_msat > 0);

    // Upload a modified state → same backup id, version 2.
    let mut v2_data = TEST_BACKUP.to_vec();
    v2_data.extend_from_slice(b"\n// channel state update");
    let up2 = client
        .upload(
            "test-backup.json",
            BackupKind::Lightning,
            "",
            v2_data.clone(),
        )
        .await
        .unwrap();
    assert_eq!(up2.manifest.backup_id, up1.manifest.backup_id);
    assert_eq!(up2.stored_version.version, 2);
    assert_eq!(up2.manifest.versions.len(), 2);
    // v2 content differs → different torrent infohash.
    assert_ne!(up1.stored_version.infohash, up2.stored_version.infohash);

    let id = up1.manifest.backup_id.clone();

    // Both versions restorable, byte-exact.
    assert_eq!(client.fetch_data(&id, 1).await.unwrap(), TEST_BACKUP);
    assert_eq!(client.fetch_data(&id, 2).await.unwrap(), v2_data);

    // Proof-of-storage on latest (v2) verifies against our local copy...
    let outcome = client.verify(&id, &v2_data, None).await.unwrap();
    assert!(outcome.ok);
    assert_eq!(outcome.version, 2);

    // ...and fails against the wrong local data (v1 vs latest v2).
    let stale = client.verify(&id, TEST_BACKUP, None).await.unwrap();
    assert!(!stale.ok);

    // Explicit historical version still provable.
    let hist = client.verify(&id, TEST_BACKUP, Some(1)).await.unwrap();
    assert!(hist.ok);

    // A proof for one challenge can't satisfy a different one (fresh nonce).
    let ch_a = make_challenge(v2_data.len() as u64, 3);
    let ch_b = make_challenge(v2_data.len() as u64, 3);
    let resp_a = client.challenge(&id, &ch_a, None).await.unwrap();
    assert!(verify_proof(&ch_a, &v2_data, &resp_a.proof));
    assert!(!verify_proof(&ch_b, &v2_data, &resp_a.proof));

    // Proof OK → client releases payment; server records earnings.
    let payment = client
        .pay_mock(&id, up2.quote_msat, Some("watch round 1".into()))
        .await
        .unwrap();
    assert_eq!(payment.amount_msat, up2.quote_msat);

    let info = client.info().await.unwrap();
    assert_eq!(info["payments_count"], 1);
    assert_eq!(info["payments_total_msat"], up2.quote_msat);
    assert_eq!(info["backups_count"], 1);

    // Unknown backup 404s.
    assert!(client.fetch_data("ffffffffffffffff", 1).await.is_err());
}
