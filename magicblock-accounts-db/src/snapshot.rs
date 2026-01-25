use std::{
    collections::VecDeque,
    ffi::OsStr,
    fs::{self, File},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::{Mutex, RwLockWriteGuard};
use tracing::{error, info, warn};

use crate::{
    error::{AccountsDbError, LogErr},
    storage::ACCOUNTS_DB_FILENAME,
    AccountsDbResult,
};

/// Defines the mechanism used to persist snapshots to disk.
#[derive(Debug, Clone, Copy)]
enum SnapshotStrategy {
    /// Utilizes filesystem CoW (Copy-on-Write) features (e.g., `ioctl_ficlonerange` on Linux).
    /// This is an O(1) operation regarding data size.
    Reflink,
    /// Fallback for standard filesystems. Performs a deep recursive copy of the directory.
    /// Requires capturing the `mmap` state into RAM before writing to ensure consistency.
    LegacyCopy,
}

impl SnapshotStrategy {
    /// Probes the filesystem at `dir` to determine if CoW operations are supported.
    fn detect(dir: &Path) -> AccountsDbResult<Self> {
        if fs_backend::supports_reflink(dir).log_err(|| "CoW check failed")? {
            info!("Snapshot Strategy: Reflink (Fast/CoW)");
            Ok(Self::Reflink)
        } else {
            warn!("Snapshot Strategy: Deep Copy (Slow)");
            Ok(Self::LegacyCopy)
        }
    }

    /// Executes the snapshot operation based on the active strategy.
    ///
    /// # Arguments
    /// * `memory_state` - Required only for `LegacyCopy`. Contains the byte-consistent view
    ///   of the main database file.
    /// * `lock` - Write lock, which prevents any accountsdb modifications during snapshotting
    fn execute(
        &self,
        src: &Path,
        dst: &Path,
        memory_state: Vec<u8>,
        lock: RwLockWriteGuard<()>,
    ) -> io::Result<()> {
        match self {
            Self::Reflink => fs_backend::reflink_dir(src, dst),
            Self::LegacyCopy => {
                // Drop lock for slow copy to avoid stalling the system
                drop(lock);
                fs_backend::deep_copy_dir(src, dst, &memory_state)
            }
        }
    }
}

/// Manages the lifecycle, creation, and restoration of database snapshots.
///
/// This type handles the complexity of filesystem capabilities
/// (CoW vs Copy) and ensures atomic restoration of state.
#[derive(Debug)]
pub struct SnapshotManager {
    /// Path to the active (hot) database directory.
    db_path: PathBuf,
    /// Directory where snapshots are stored.
    snapshots_dir: PathBuf,
    /// The persistence strategy chosen during initialization.
    strategy: SnapshotStrategy,
    /// Ordered registry of valid snapshot paths (oldest to newest).
    registry: Mutex<VecDeque<PathBuf>>,
    /// Maximum number of snapshots to retain before rotating out old ones.
    max_snapshots: usize,
}

impl SnapshotManager {
    /// Initializes the manager, detecting filesystem capabilities and recovering
    /// existing snapshots from disk.
    pub fn new(
        db_path: PathBuf,
        max_snapshots: usize,
    ) -> AccountsDbResult<Arc<Self>> {
        let snapshots_dir = db_path
            .parent()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "Invalid db_path")
            })
            .log_err(|| "Failed to resolve snapshots directory")?
            .to_path_buf();

        // 1. Determine capabilities (CoW or Deep Copy)
        let strategy = SnapshotStrategy::detect(&snapshots_dir)?;

        // 2. Load and sort existing snapshots from the filesystem
        let registry = Self::recover_registry(&snapshots_dir, max_snapshots)
            .log_err(|| "Failed to load snapshot registry")?;

        Ok(Arc::new(Self {
            db_path,
            snapshots_dir,
            strategy,
            registry: Mutex::new(registry),
            max_snapshots,
        }))
    }

    /// Creates a durable snapshot of the current database state for the given `slot`.
    ///
    /// # Locking & Consistency
    /// * **CoW (Reflink)**: The `_write_lock` is held during the reflink operation.
    ///   Since reflink is a metadata operation, this is extremely fast.
    /// * **Legacy Copy**: The `_write_lock` is dropped *after* capturing the `active_mem`
    ///   state to RAM, but *before* the slow disk I/O. This prevents blocking the validator
    ///   during the deep copy.
    pub fn create_snapshot(
        &self,
        slot: u64,
        active_mem: &[u8],
        lock: RwLockWriteGuard<()>,
    ) -> AccountsDbResult<()> {
        let snap_path = self.generate_path(slot);

        // 1. Maintain retention policy
        self.prune_registry();

        // 2. Prepare Data Capture
        // If legacy copy, we must capture state while lock is held.
        let memory_capture =
            matches!(self.strategy, SnapshotStrategy::LegacyCopy)
                .then(|| active_mem.to_vec())
                .unwrap_or_default();

        // 3. Execute Snapshot
        self.strategy
            .execute(&self.db_path, &snap_path, memory_capture, lock)
            .log_err(|| "Snapshot failed")?;

        // 4. Register success
        self.registry.lock().push_back(snap_path);
        Ok(())
    }

    /// Atomically restores the database to the snapshot nearest to `target_slot`.
    ///
    /// # Critical Operation
    /// This function replaces the active database directory.
    /// 1. Finds the best candidate snapshot (<= target_slot).
    /// 2. Prunes all snapshots newer than the candidate (invalidated history).
    /// 3. Performs an atomic swap: Active -> Backup, Snapshot -> Active.
    ///
    /// Returns the actual slot number of the restored snapshot.
    pub fn restore_from_snapshot(
        &self,
        target_slot: u64,
    ) -> AccountsDbResult<u64> {
        let mut registry = self.registry.lock();

        // 1. Locate Snapshot (Binary Search)
        let search_key = self.generate_path(target_slot);
        let index = match registry.binary_search(&search_key) {
            Ok(i) => i,
            Err(i) if i > 0 => i - 1,
            _ => return Err(AccountsDbError::SnapshotMissing(target_slot)),
        };

        let chosen_path = registry.remove(index).unwrap();
        let chosen_slot = Self::parse_slot(&chosen_path)
            .ok_or(AccountsDbError::SnapshotMissing(target_slot))?;

        info!(
            chosen_slot = chosen_slot,
            target_slot = target_slot,
            "Restoring snapshot"
        );

        // 2. Prune Invalidated Futures
        // Any snapshot strictly newer than the chosen one is now on a diverging timeline.
        for invalidated in registry.drain(index..) {
            warn!(
                invalidated_path = %invalidated.display(),
                "Pruning invalidated snapshot"
            );
            let _ = fs::remove_dir_all(&invalidated);
        }

        // 3. Atomic Swapping via Rename
        let backup = self.db_path.with_extension("bak");

        // Stage current DB as backup
        if self.db_path.exists() {
            fs::rename(&self.db_path, &backup)
                .log_err(|| "Failed to stage backup")?;
        }

        // Promote snapshot to active
        if let Err(e) = fs::rename(&chosen_path, &self.db_path) {
            error!(
                error = ?e,
                "Restore failed during promote"
            );
            // Attempt to restore the backup if promotion fails
            if backup.exists() {
                let _ = fs::rename(&backup, &self.db_path);
            }
            return Err(e.into());
        }

        // Success: remove backup
        let _ = fs::remove_dir_all(&backup);
        Ok(chosen_slot)
    }

    /// Checks if a snapshot for the exact `slot` exists in the registry.
    #[cfg(test)]
    pub fn snapshot_exists(&self, slot: u64) -> bool {
        let path = self.generate_path(slot);
        self.registry.lock().binary_search(&path).is_ok()
    }

    // --- Private Helpers ---

    fn generate_path(&self, slot: u64) -> PathBuf {
        // Padding ensures standard string sorting aligns with numeric sorting
        self.snapshots_dir.join(format!("snapshot-{:0>12}", slot))
    }

    fn parse_slot(path: &Path) -> Option<u64> {
        path.file_name()
            .and_then(OsStr::to_str)
            .and_then(|s| s.strip_prefix("snapshot-"))
            .and_then(|s| s.parse().ok())
    }

    /// Removes the oldest snapshots until `registry.len() < max_snapshots`.
    fn prune_registry(&self) {
        let mut registry = self.registry.lock();
        while registry.len() >= self.max_snapshots {
            if let Some(path) = registry.pop_front() {
                if let Err(e) = fs::remove_dir_all(&path) {
                    warn!(
                        path = %path.display(),
                        error = ?e,
                        "Failed to prune snapshot"
                    );
                }
            }
        }
    }

    /// Scans the disk for existing snapshot directories and builds the registry.
    fn recover_registry(
        dir: &Path,
        max: usize,
    ) -> io::Result<VecDeque<PathBuf>> {
        if !dir.exists() {
            fs::create_dir_all(dir)?;
            return Ok(VecDeque::new());
        }

        let mut paths: Vec<_> = fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir() && Self::parse_slot(p).is_some())
            .collect();

        paths.sort(); // Sorting works due to zero-padded filenames

        // Enforce limit strictly on startup
        let offset = paths.len().saturating_sub(max);
        Ok(paths.into_iter().skip(offset).collect())
    }

    /// Return the path to main accountsdb directory
    pub(crate) fn database_path(&self) -> &Path {
        &self.db_path
    }
}

mod fs_backend {
    use super::*;

    /// Writes a test file and attempts a reflink to determine CoW support.
    pub(crate) fn supports_reflink(dir: &Path) -> io::Result<bool> {
        let src = dir.join(".tmp_cow_probe_src");
        let dst = dir.join(".tmp_cow_probe_dst");

        // Clean slate
        let _ = fs::remove_file(&src);
        let _ = fs::remove_file(&dst);

        {
            let mut f = File::create(&src)?;
            f.write_all(&[0u8; 64])?;
            f.sync_data()?;
        }

        let result = reflink::reflink(&src, &dst).is_ok();

        let _ = fs::remove_file(src);
        let _ = fs::remove_file(dst);

        Ok(result)
    }

    pub(crate) fn reflink_dir(src: &Path, dst: &Path) -> io::Result<()> {
        // If src is a directory, manually copy entries
        if src.is_dir() {
            fs::create_dir_all(dst)?;
            for entry in fs::read_dir(src)? {
                let entry = entry?;
                let src_path = entry.path();
                let dst_path = dst.join(entry.file_name());

                if entry.file_type()?.is_dir() {
                    reflink_dir(&src_path, &dst_path)?;
                } else {
                    reflink::reflink(&src_path, &dst_path)?
                }
            }
            Ok(())
        } else {
            reflink::reflink(src, dst)
        }
    }

    /// Recursively copies a directory.
    ///
    /// Special Handling:
    /// If `ACCOUNTS_DB_FILENAME` is encountered, it writes the provided `mem_dump`
    /// instead of copying the file from disk. This ensures we write the state
    /// consistent with the capture time, ignoring any subsequent disk modifications.
    pub(crate) fn deep_copy_dir(
        src: &Path,
        dst: &Path,
        mem_dump: &[u8],
    ) -> io::Result<()> {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());

            if ty.is_dir() {
                deep_copy_dir(&src_path, &dst_path, mem_dump)?;
            } else if src_path.file_name().and_then(OsStr::to_str)
                == Some(ACCOUNTS_DB_FILENAME)
            {
                write_dump_file(&dst_path, mem_dump)?;
            } else {
                fs::copy(&src_path, &dst_path)?;
            }
        }
        Ok(())
    }

    /// Writes the memory capture to disk using buffered I/O.
    fn write_dump_file(path: &Path, data: &[u8]) -> io::Result<()> {
        let f = File::create(path)?;
        // Pre-allocate space if OS supports it to reduce fragmentation
        f.set_len(data.len() as u64)?;

        let mut writer = BufWriter::new(f);
        writer.write_all(data)?;
        writer.flush()?;
        writer.get_mut().sync_all()?;
        Ok(())
    }
}
