use std::{
    collections::VecDeque,
    ffi::OsStr,
    fs::{self, File},
    io::{self, BufWriter, Cursor, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use parking_lot::{Mutex, RwLockWriteGuard};
use tar::{Archive, Builder};
use tracing::{error, info, warn};

use crate::{
    error::{AccountsDbError, LogErr},
    storage::ACCOUNTS_DB_FILENAME,
    AccountsDbResult,
};

const SNAPSHOT_PREFIX: &str = "snapshot-";
const ARCHIVE_EXT: &str = "tar.gz";

/// Snapshot persistence strategy.
#[derive(Debug, Clone, Copy)]
enum SnapshotStrategy {
    /// CoW reflink - O(1) metadata operation when filesystem supports it.
    Reflink,
    /// Deep copy with memory capture for consistency.
    LegacyCopy,
}

impl SnapshotStrategy {
    fn detect(dir: &Path) -> AccountsDbResult<Self> {
        let supports_cow =
            fs_backend::supports_reflink(dir).log_err(|| "CoW check failed")?;
        if supports_cow {
            info!("Snapshot Strategy: Reflink (Fast/CoW)");
            Ok(Self::Reflink)
        } else {
            warn!("Snapshot Strategy: Deep Copy (Slow)");
            Ok(Self::LegacyCopy)
        }
    }

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
                drop(lock); // Release lock before slow I/O
                fs_backend::deep_copy_dir(src, dst, &memory_state)
            }
        }
    }
}

/// Manages snapshot lifecycle: creation, archival, and restoration.
///
/// Snapshots are stored as compressed tar archives (`.tar.gz`). During creation,
/// a directory is first created, then archived asynchronously after releasing
/// the write lock to minimize blocking time.
#[derive(Debug)]
pub struct SnapshotManager {
    db_path: PathBuf,
    snapshots_dir: PathBuf,
    strategy: SnapshotStrategy,
    /// Ordered registry of archive paths (oldest to newest).
    registry: Mutex<VecDeque<PathBuf>>,
    max_snapshots: usize,
}

impl SnapshotManager {
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

        let strategy = SnapshotStrategy::detect(&snapshots_dir)?;
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

    /// Creates a snapshot directory. Returns the path for later archiving.
    pub fn create_snapshot_dir(
        &self,
        slot: u64,
        active_mem: &[u8],
        lock: RwLockWriteGuard<()>,
    ) -> AccountsDbResult<PathBuf> {
        self.prune_registry();

        let snap_path = self.slot_to_dir_path(slot);
        let memory_capture =
            matches!(self.strategy, SnapshotStrategy::LegacyCopy)
                .then(|| active_mem.to_vec())
                .unwrap_or_default();

        self.strategy
            .execute(&self.db_path, &snap_path, memory_capture, lock)
            .log_err(|| "Snapshot failed")?;

        Ok(snap_path)
    }

    /// Archives the snapshot directory to `.tar.gz` and removes the directory.
    pub fn archive_and_register(
        &self,
        snapshot_dir: &Path,
    ) -> AccountsDbResult<PathBuf> {
        let archive_path = snapshot_dir.with_extension(ARCHIVE_EXT);

        info!(archive_path = %archive_path.display(), "Archiving snapshot");

        let file = File::create(&archive_path).log_err(|| {
            format!("Failed to create archive at {}", archive_path.display())
        })?;
        let enc = GzEncoder::new(file, Compression::fast());
        let mut tar = Builder::new(enc);
        tar.append_dir_all(".", snapshot_dir)
            .log_err(|| "Failed to append directory to tar")?;
        tar.finish().log_err(|| "Failed to finalize tar archive")?;

        fs::remove_dir_all(snapshot_dir).log_err(|| {
            format!(
                "Failed to remove snapshot directory {}",
                snapshot_dir.display()
            )
        })?;

        self.register_archive(archive_path.clone());
        Ok(archive_path)
    }

    /// Inserts an external snapshot archive received from network.
    ///
    /// If `slot > current_slot`, immediately fast-forwards to the snapshot.
    /// Returns `true` if fast-forward was performed.
    pub fn insert_external_snapshot(
        &self,
        slot: u64,
        archive_bytes: &[u8],
        current_slot: u64,
    ) -> AccountsDbResult<bool> {
        // Validate archive structure
        Self::validate_archive(archive_bytes)?;

        let archive_path = self.slot_to_archive_path(slot);
        if archive_path.exists() {
            return Err(AccountsDbError::Internal(format!(
                "Snapshot for slot {} already exists",
                slot
            )));
        }

        info!(slot, "Inserting external snapshot");

        // Write archive to disk
        let mut file = File::create(&archive_path).log_err(|| {
            format!("Failed to create archive at {}", archive_path.display())
        })?;
        file.write_all(archive_bytes)
            .log_err(|| "Failed to write archive bytes")?;
        file.sync_all()
            .log_err(|| "Failed to sync archive to disk")?;

        // Fast-forward if snapshot is newer than current state
        if slot > current_slot {
            info!(slot, current_slot, "Fast-forwarding to external snapshot");
            self.fast_forward(slot, &archive_path)?;
            return Ok(true);
        }

        // Otherwise just register for later use
        self.prune_registry();
        self.register_archive(archive_path);
        Ok(false)
    }

    /// Restores database to the snapshot nearest to `target_slot`.
    pub fn restore_from_snapshot(
        &self,
        target_slot: u64,
    ) -> AccountsDbResult<u64> {
        let (chosen_archive, chosen_slot, index) =
            self.find_and_remove_snapshot(target_slot)?;

        let extracted_dir = self.extract_archive(&chosen_archive)?;
        self.atomic_swap(&extracted_dir)?;
        self.prune_invalidated_snapshots(index);
        let _ = fs::remove_file(&chosen_archive);

        Ok(chosen_slot)
    }

    /// Validates that bytes represent a valid gzip tar archive.
    fn validate_archive(bytes: &[u8]) -> AccountsDbResult<()> {
        let cursor = Cursor::new(bytes);
        let dec = GzDecoder::new(cursor);
        let mut tar = Archive::new(dec);
        tar.entries()
            .log_err(|| "Invalid snapshot archive: not a valid gzip tar")?;
        Ok(())
    }

    /// Finds the best snapshot for target_slot, removes from registry.
    /// Returns (archive_path, slot, index).
    fn find_and_remove_snapshot(
        &self,
        target_slot: u64,
    ) -> AccountsDbResult<(PathBuf, u64, usize)> {
        let mut registry = self.registry.lock();
        let search_key = self.slot_to_archive_path(target_slot);
        let index = match registry.binary_search(&search_key) {
            Ok(i) => i,
            Err(i) if i > 0 => i - 1,
            _ => return Err(AccountsDbError::SnapshotMissing(target_slot)),
        };

        let chosen_archive = registry.remove(index).unwrap();
        let chosen_slot = Self::parse_slot(&chosen_archive)
            .ok_or(AccountsDbError::SnapshotMissing(target_slot))?;

        info!(chosen_slot, target_slot, "Restoring snapshot");
        Ok((chosen_archive, chosen_slot, index))
    }

    /// Extracts a tar.gz archive to a temporary directory.
    fn extract_archive(
        &self,
        archive_path: &Path,
    ) -> AccountsDbResult<PathBuf> {
        let extract_dir = archive_path.with_extension("extract");

        info!(archive_path = %archive_path.display(), "Extracting snapshot archive");

        let file = File::open(archive_path).log_err(|| {
            format!("Failed to open archive {}", archive_path.display())
        })?;
        let mut tar = Archive::new(GzDecoder::new(file));
        tar.unpack(&extract_dir).log_err(|| {
            format!("Failed to extract archive to {}", extract_dir.display())
        })?;

        Ok(extract_dir)
    }

    /// Performs atomic swap: current db -> backup, extracted -> current db.
    /// On failure, rolls back to original state.
    fn atomic_swap(&self, extracted_dir: &Path) -> AccountsDbResult<()> {
        let backup = self.db_path.with_extension("bak");

        if self.db_path.exists() {
            fs::rename(&self.db_path, &backup)
                .log_err(|| "Failed to stage backup")?;
        }

        if let Err(e) = fs::rename(extracted_dir, &self.db_path) {
            error!(error = ?e, "Atomic swap failed during promote");
            if backup.exists() {
                let _ = fs::rename(&backup, &self.db_path);
            }
            let _ = fs::remove_dir_all(extracted_dir);
            return Err(e.into());
        }

        let _ = fs::remove_dir_all(&backup);
        Ok(())
    }

    /// Fast-forwards to a snapshot that's newer than current state.
    fn fast_forward(
        &self,
        slot: u64,
        archive_path: &Path,
    ) -> AccountsDbResult<()> {
        let extracted_dir = self.extract_archive(archive_path)?;
        self.atomic_swap(&extracted_dir)?;
        self.registry.lock().push_back(archive_path.to_path_buf());
        info!(slot, "Fast-forward complete");
        Ok(())
    }

    /// Registers an archive in the registry.
    fn register_archive(&self, archive_path: PathBuf) {
        info!(archive_path = %archive_path.display(), "Snapshot registered");
        self.registry.lock().push_back(archive_path);
    }

    /// Removes snapshots newer than the chosen one (invalidated by rollback).
    fn prune_invalidated_snapshots(&self, from_index: usize) {
        let mut registry = self.registry.lock();
        for invalidated in registry.drain(from_index..) {
            warn!(invalidated_path = %invalidated.display(), "Pruning invalidated snapshot");
            let _ = fs::remove_file(&invalidated);
        }
    }

    /// Removes oldest snapshots until under the limit.
    fn prune_registry(&self) {
        let mut registry = self.registry.lock();
        while registry.len() >= self.max_snapshots {
            let Some(path) = registry.pop_front() else {
                break;
            };
            if let Err(e) = fs::remove_file(&path) {
                warn!(path = %path.display(), error = ?e, "Failed to prune snapshot archive");
            }
        }
    }

    /// Scans disk for existing snapshot archives.
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
            .filter(|p| {
                p.is_file()
                    && p.extension().is_some_and(|e| e == "gz")
                    && Self::parse_slot(p).is_some()
            })
            .collect();
        paths.sort();

        // Clean orphan directories (interrupted archiving)
        for entry in fs::read_dir(dir)?.filter_map(|e| e.ok()) {
            let path = entry.path();
            let name = path.file_name().and_then(OsStr::to_str);
            if path.is_dir()
                && name.is_some_and(|n| {
                    n.starts_with(SNAPSHOT_PREFIX) && !n.contains('.')
                })
            {
                warn!(path = %path.display(), "Cleaning up orphan snapshot directory");
                let _ = fs::remove_dir_all(&path);
            }
        }

        let offset = paths.len().saturating_sub(max);
        Ok(paths.into_iter().skip(offset).collect())
    }

    fn slot_to_dir_path(&self, slot: u64) -> PathBuf {
        self.snapshots_dir
            .join(format!("{SNAPSHOT_PREFIX}{slot:0>12}"))
    }

    fn slot_to_archive_path(&self, slot: u64) -> PathBuf {
        self.snapshots_dir
            .join(format!("{SNAPSHOT_PREFIX}{slot:0>12}.{ARCHIVE_EXT}"))
    }

    fn parse_slot(path: &Path) -> Option<u64> {
        path.file_name()?
            .to_str()?
            .strip_prefix(SNAPSHOT_PREFIX)?
            .strip_suffix(&format!(".{ARCHIVE_EXT}"))?
            .parse()
            .ok()
    }

    pub(crate) fn database_path(&self) -> &Path {
        &self.db_path
    }

    #[cfg(test)]
    pub fn snapshot_exists(&self, slot: u64) -> bool {
        self.registry
            .lock()
            .binary_search(&self.slot_to_archive_path(slot))
            .is_ok()
    }
}

mod fs_backend {
    use super::*;

    pub(crate) fn supports_reflink(dir: &Path) -> io::Result<bool> {
        let src = dir.join(".tmp_cow_probe_src");
        let dst = dir.join(".tmp_cow_probe_dst");
        let _ = (fs::remove_file(&src), fs::remove_file(&dst));

        File::create(&src)?.write_all(&[0u8; 64])?;
        let result = reflink::reflink(&src, &dst).is_ok();
        let _ = (fs::remove_file(src), fs::remove_file(dst));
        Ok(result)
    }

    pub(crate) fn reflink_dir(src: &Path, dst: &Path) -> io::Result<()> {
        if !src.is_dir() {
            return reflink::reflink(src, dst);
        }
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let (src_path, dst_path) =
                (entry.path(), dst.join(entry.file_name()));
            if entry.file_type()?.is_dir() {
                reflink_dir(&src_path, &dst_path)?;
            } else {
                reflink::reflink(&src_path, &dst_path)?;
            }
        }
        Ok(())
    }

    /// Deep copy with special handling: writes `mem_dump` for `accounts.db`
    /// instead of copying from disk to ensure consistency.
    pub(crate) fn deep_copy_dir(
        src: &Path,
        dst: &Path,
        mem_dump: &[u8],
    ) -> io::Result<()> {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let (src_path, dst_path) =
                (entry.path(), dst.join(entry.file_name()));
            if entry.file_type()?.is_dir() {
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

    fn write_dump_file(path: &Path, data: &[u8]) -> io::Result<()> {
        let f = File::create(path)?;
        f.set_len(data.len() as u64)?;
        let mut writer = BufWriter::new(f);
        writer.write_all(data)?;
        writer.flush()?;
        writer.get_mut().sync_all()
    }
}
