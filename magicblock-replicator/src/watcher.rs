//! Directory watcher for AccountsDb snapshot archives.
//!
//! Monitors a directory for new `.tar.gz` snapshot files and yields them
//! as open [`tokio::fs::File`] handles via a channel for tokio::select compatibility.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::{fs::File, sync::mpsc};
use tracing::{error, info};

use crate::Result;

const SNAPSHOT_EXTENSION: &str = "tar.gz";
const SNAPSHOT_PREFIX: &str = "snapshot-";
const OPEN_RETRY_DELAY: Duration = Duration::from_millis(50);
const OPEN_RETRIES: usize = 8;

/// Extracts the slot number from a snapshot filename.
///
/// Expected format: `snapshot-{slot:0>12}.tar.gz`
/// Example: `snapshot-000000000001.tar.gz` -> `Some(1)`
pub fn parse_slot(path: &Path) -> Option<u64> {
    path.file_name()?
        .to_str()?
        .strip_prefix(SNAPSHOT_PREFIX)?
        .strip_suffix(&format!(".{SNAPSHOT_EXTENSION}"))?
        .parse()
        .ok()
}

/// Watcher for snapshot archive files in a directory.
///
/// Uses `notify` for filesystem events and yields open file handles
/// via an mpsc channel compatible with `tokio::select!`.
pub struct SnapshotWatcher {
    _watcher: RecommendedWatcher,
    rx: mpsc::Receiver<PathBuf>,
    last_slot: Option<u64>,
}

impl SnapshotWatcher {
    /// Creates a new watcher monitoring the given directory.
    ///
    /// The watcher detects newly created `.tar.gz` files and opens them
    /// for reading when [`Self::recv`] is called.
    ///
    /// # Errors
    ///
    /// Returns an error if the watcher cannot be initialized or the
    /// directory cannot be accessed.
    pub fn new(dir: &Path) -> Result<Self> {
        let (tx, rx) = mpsc::channel(32);

        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| {
                let Ok(event) = res else {
                    return;
                };
                let Some(path) = Self::process_event(&event) else {
                    return;
                };
                if let Err(e) = tx.blocking_send(path) {
                    error!("Failed to send snapshot event: {}", e);
                }
            })?;

        watcher.watch(dir, RecursiveMode::NonRecursive)?;
        info!(dir = %dir.display(), "Snapshot watcher started");

        Ok(Self {
            _watcher: watcher,
            rx,
            last_slot: None,
        })
    }

    /// Process a filesystem event and extract snapshot path if relevant.
    fn process_event(event: &Event) -> Option<PathBuf> {
        if !matches!(event.kind, EventKind::Create(_)) {
            return None;
        }

        for path in &event.paths {
            if Self::is_snapshot_file(path) {
                return Some(path.clone());
            }
        }

        None
    }

    /// Check if a path is a snapshot archive file.
    fn is_snapshot_file(path: &std::path::Path) -> bool {
        path.is_file()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&format!(".{SNAPSHOT_EXTENSION}")))
    }

    /// Receive the next detected snapshot as an open file handle and slot.
    ///
    /// Opens the file for reading before returning. This method is
    /// `tokio::select!` compatible. Returns `None` when the watcher
    /// has been dropped.
    pub async fn recv(&mut self) -> Option<(File, u64)> {
        loop {
            let path = self.rx.recv().await?;
            let Some(slot) = parse_slot(&path) else {
                continue;
            };

            if self.last_slot.is_some_and(|last| slot <= last) {
                continue;
            }

            let Some(file) = Self::open_with_retry(&path).await else {
                continue;
            };
            self.last_slot = Some(slot);
            break Some((file, slot));
        }
    }

    async fn open_with_retry(path: &Path) -> Option<File> {
        for _ in 0..OPEN_RETRIES {
            if let Ok(file) = File::open(path).await {
                return Some(file);
            }

            tokio::time::sleep(OPEN_RETRY_DELAY).await;
        }

        None
    }
}
