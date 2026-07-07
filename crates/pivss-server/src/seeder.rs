//! Torrent seeding behind a trait.
//!
//! [`CarlSeeder`] shells out to [carl](https://github.com/vincenzopalazzo/carl)
//! (`carl seed <file.torrent> <data-dir> --port N`) and keeps the child
//! processes alive for the lifetime of the server. [`NoopSeeder`] is used when
//! seeding is disabled or carl isn't installed — torrents and magnet links are
//! still created either way, so any BitTorrent client can seed the
//! `<data_dir>/seeds` directory out of band.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::process::{Child, Command};

#[derive(Debug, Clone, serde::Serialize)]
pub struct SeedStatus {
    pub infohash: String,
    pub torrent_path: PathBuf,
    pub seeding: bool,
    pub detail: String,
}

#[async_trait::async_trait]
pub trait Seeder: Send + Sync {
    /// Start (or record) seeding for a torrent file whose payload lives in `data_dir`.
    async fn seed(&self, infohash: &str, torrent_path: &Path, data_dir: &Path) -> SeedStatus;

    fn statuses(&self) -> Vec<SeedStatus>;

    fn name(&self) -> &'static str;
}

pub struct NoopSeeder {
    statuses: Mutex<Vec<SeedStatus>>,
    reason: String,
}

impl NoopSeeder {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            statuses: Mutex::new(vec![]),
            reason: reason.into(),
        }
    }
}

#[async_trait::async_trait]
impl Seeder for NoopSeeder {
    async fn seed(&self, infohash: &str, torrent_path: &Path, _data_dir: &Path) -> SeedStatus {
        let status = SeedStatus {
            infohash: infohash.to_string(),
            torrent_path: torrent_path.to_path_buf(),
            seeding: false,
            detail: self.reason.clone(),
        };
        self.statuses.lock().unwrap().push(status.clone());
        status
    }

    fn statuses(&self) -> Vec<SeedStatus> {
        self.statuses.lock().unwrap().clone()
    }

    fn name(&self) -> &'static str {
        "noop"
    }
}

pub struct CarlSeeder {
    carl_bin: String,
    base_port: u16,
    children: Mutex<HashMap<String, Child>>,
    statuses: Mutex<Vec<SeedStatus>>,
}

impl CarlSeeder {
    /// Returns None if the carl binary is not runnable.
    pub async fn detect(carl_bin: &str, base_port: u16) -> Option<Self> {
        let ok = Command::new(carl_bin)
            .arg("--help")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);
        ok.then(|| Self {
            carl_bin: carl_bin.to_string(),
            base_port,
            children: Mutex::new(HashMap::new()),
            statuses: Mutex::new(vec![]),
        })
    }
}

#[async_trait::async_trait]
impl Seeder for CarlSeeder {
    async fn seed(&self, infohash: &str, torrent_path: &Path, data_dir: &Path) -> SeedStatus {
        let port = self.base_port + (self.children.lock().unwrap().len() as u16);
        let spawned = Command::new(&self.carl_bin)
            .arg("seed")
            .arg(torrent_path)
            .arg(data_dir)
            .arg("--port")
            .arg(port.to_string())
            .kill_on_drop(true)
            .spawn();

        let status = match spawned {
            Ok(child) => {
                self.children
                    .lock()
                    .unwrap()
                    .insert(infohash.to_string(), child);
                SeedStatus {
                    infohash: infohash.to_string(),
                    torrent_path: torrent_path.to_path_buf(),
                    seeding: true,
                    detail: format!("carl seeding on port {port}"),
                }
            }
            Err(e) => SeedStatus {
                infohash: infohash.to_string(),
                torrent_path: torrent_path.to_path_buf(),
                seeding: false,
                detail: format!("failed to spawn carl: {e}"),
            },
        };
        self.statuses.lock().unwrap().push(status.clone());
        status
    }

    fn statuses(&self) -> Vec<SeedStatus> {
        self.statuses.lock().unwrap().clone()
    }

    fn name(&self) -> &'static str {
        "carl"
    }
}
