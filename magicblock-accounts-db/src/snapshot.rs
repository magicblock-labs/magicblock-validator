use std::{
    collections::VecDeque,
    ffi::OsStr,
    fs,
    fs::File,
    io,
    io::Write,
    path::{Path, PathBuf},
};

use log::{info, warn};
use memmap2::MmapMut;
use parking_lot::Mutex;
use reflink::reflink;

use crate::{error::AccountsDbError, log_err, storage::ADB_FILE, AdbResult};

pub struct SnapshotEngine {
    /// directory path where database files are kept
    dbpath: PathBuf,
    /// indicator flag for Copy on Write support on host file system
    is_cow_supported: bool,
    /// List of existing snapshots
    /// Note: as it's locked only when slot is incremented
    /// this is basically a contention free Mutex we use it
    /// for the convenience of interior mutability
    snapshots: Mutex<VecDeque<PathBuf>>,
    /// max number of snapshots to keep alive
    max_count: usize,
}

impl SnapshotEngine {
    pub(crate) fn new(
        dbpath: PathBuf,
        max_count: usize,
    ) -> AdbResult<Box<Self>> {
        let is_cow_supported = Self::supports_cow(&dbpath)
            .inspect_err(log_err!("cow support check"))?;
        let snapshots = Self::read_snapshots(&dbpath, max_count)?.into();

        Ok(Box::new(Self {
            dbpath,
            is_cow_supported,
            snapshots,
            max_count,
        }))
    }

    /// Take snapshot of database directory, this operation
    /// assumes that no writers are currently active
    pub(crate) fn snapshot(&self, slot: u64, mmap: &[u8]) -> AdbResult<()> {
        let slot = SnapSlot(slot);
        // this lock is always free, as we take StWLock higher up in the call stack and
        // only one thread can take snapshots, namely the one that advances the slot
        let mut snapshots = self.snapshots.lock();
        if snapshots.len() == self.max_count {
            if let Some(old) = snapshots.pop_front() {
                let _ = fs::remove_dir_all(&old)
                    .inspect_err(log_err!("error during old snapshot removal"));
            }
        }
        let snapout = slot.as_path(Self::snapshots_dir(&self.dbpath));

        if self.is_cow_supported {
            self.reflink_dir(&snapout)?;
        } else {
            rcopy_dir(&self.dbpath, &snapout, mmap)?;
        }
        snapshots.push_back(snapout);
        Ok(())
    }

    /// Try to rollback to snapshot which is the most recent one before given slot
    ///
    /// NOTE: In case of success, this deletes the primary
    /// database, and all newer snapshots, use carefully!
    pub(crate) fn try_switch_to_snapshot(
        &self,
        mut slot: u64,
    ) -> AdbResult<u64> {
        let mut spath =
            SnapSlot(slot).as_path(Self::snapshots_dir(&self.dbpath));
        let mut snapshots = self.snapshots.lock(); // free lock

        // paths to snapshots are strictly ordered, so we can b-search
        let index = match snapshots.binary_search(&spath) {
            Ok(i) => i,
            // if we have snapshot older than the slot, use it
            Err(i) if i != 0 => i - 1,
            // otherwise we don't have any snapshot before the given slot
            Err(_) => return Err(AccountsDbError::SnapshotMissing(slot)),
        };

        // SAFETY:
        // we just checked the index above, so this cannot fail
        spath = snapshots.swap_remove_back(index).unwrap();
        info!(
            "rolling back to snapshot before {slot} using {}",
            spath.display()
        );

        // remove all newer snapshots
        while let Some(path) = snapshots.swap_remove_back(index) {
            warn!("removing snapshot at {}", path.display());
            // if this operation fails (which is unlikely), then it most likely failed due to
            // the path being invalid, which is fine by us, since we wanted to remove it anyway
            let _ = fs::remove_dir_all(path)
                .inspect_err(log_err!("error removing snapshot"));
        }

        // SAFETY:
        // infallible, all entries in `snapshots` are
        // created with SnapSlot naming conventions
        slot = SnapSlot::try_from_path(&spath).unwrap().0;

        // we perform database swap, thus removing
        // latest state and rolling back to snapshot
        fs::remove_dir_all(&self.dbpath).inspect_err(log_err!(
            "failed to remove current database at {}",
            self.dbpath.display()
        ))?;
        fs::rename(&spath, &self.dbpath).inspect_err(log_err!(
            "failed to rename snapshot dir {} -> {}",
            spath.display(),
            self.dbpath.display()
        ))?;

        Ok(slot)
    }

    #[inline]
    pub(crate) fn database_path(&self) -> &Path {
        &self.dbpath
    }

    /// Perform test to find out whether file system
    /// supports CoW operations (btrfs, xfs, zfs, apfs)
    fn supports_cow(dir: &Path) -> io::Result<bool> {
        let tmp = dir.join("__tempfile.fs");
        let mut file = File::create(&tmp)?;
        file.set_len(64)?;
        file.write_all(&[42; 64])?;
        file.flush()?;
        let tmpsnap = dir.join("__tempfile_snap.fs");
        // reflink will fail if CoW is not supported by FS
        let supported = reflink(&tmp, &tmpsnap).is_ok();
        if supported {
            info!("Host file system supports CoW, will use reflinking (fast)");
        } else {
            warn!(
                "Host file system doesn't support CoW, will use regular (slow) file copy, OK for development environments"
            );
        };
        if tmp.exists() {
            fs::remove_file(tmp)?;
        }
        // if we failed to create the file then the below operation will fail,
        // but since we wanted to remove it anyway, just ignore the error
        let _ = fs::remove_file(tmpsnap);
        Ok(supported)
    }

    fn snapshots_dir(dbpath: &Path) -> &Path {
        dbpath
            .parent()
            .expect("accounts database directory should have a parent")
    }

    /// Reads the list of snapshots directories from disk, this
    /// is necessary to restore last state after restart
    fn read_snapshots(
        dbpath: &Path,
        max_count: usize,
    ) -> io::Result<VecDeque<PathBuf>> {
        let snapdir = Self::snapshots_dir(dbpath);
        let mut snapshots = VecDeque::with_capacity(max_count);

        if !snapdir.exists() {
            fs::create_dir_all(snapdir)?;
            return Ok(snapshots);
        }
        for entry in fs::read_dir(snapdir)? {
            let snap = entry?.path();
            if snap.is_dir() && SnapSlot::try_from_path(&snap).is_some() {
                snapshots.push_back(snap);
            }
        }
        // sorting is required for correct ordering (slot-wise) of snapshots
        snapshots.make_contiguous().sort();

        while snapshots.len() > max_count {
            snapshots.pop_front();
        }
        Ok(snapshots)
    }

    /// Fast reference linking based directory copy, only works
    /// on Copy on Write filesystems like btrfs/xfs/apfs/refs, this
    /// operation is essentially a filesystem metadata update, so it usually
    /// takes a few milliseconds irrespective of the target directory size
    #[inline(always)]
    fn reflink_dir(&self, dst: &Path) -> io::Result<()> {
        reflink::reflink(&self.dbpath, dst)
    }
}

#[derive(Eq, PartialEq, PartialOrd, Ord)]
struct SnapSlot(u64);

impl SnapSlot {
    /// parse snapshot path to extract slot number
    fn try_from_path(path: &Path) -> Option<Self> {
        path.file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.split('-').nth(1))
            .and_then(|s| s.parse::<u64>().ok())
            .map(Self)
    }

    fn as_path(&self, ppath: &Path) -> PathBuf {
        // enforce strict alphanumeric ordering by introducing extra padding
        ppath.join(format!("snapshot-{:0>12}", self.0))
    }
}

/// Conventional byte to byte recursive directory copy,
/// works on all filesystems. Ideally this should only
/// be used for development purposes, and performance
/// sensitive instances of validator should run with
/// CoW supported file system for the storage needs
fn rcopy_dir(src: &Path, dst: &Path, mmap: &[u8]) -> io::Result<()> {
    fs::create_dir_all(dst).inspect_err(log_err!(
        "creating snapshot destination dir: {:?}",
        dst
    ))?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src = entry.path();
        let dst = dst.join(entry.file_name());

        if src.is_dir() {
            rcopy_dir(&src, &dst, mmap)?;
        } else if src.file_name().and_then(OsStr::to_str) == Some(ADB_FILE) {
            // for main accounts db file we have an exceptional handling logic, as this file
            // is usually huge on disk, but only a small fraction of it is actually used
            let dst = File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .read(true)
                .open(dst)
                .inspect_err(log_err!(
                    "creating a snapshot of main accounts db file"
                ))?;
            // we copy this file via mmap, only writing used portion of it, ignoring zeroes
            // NOTE: upon snapshot reload, the size will be readjusted back to the original
            // value, but for the storage purposes, we only keep actual data, ignoring slack space
            dst.set_len(mmap.len() as u64)?;
            // SAFETY:
            // we just opened and resized the file to correct length, and we will close
            // it immediately after byte copy, so no one can access it concurrently
            let mut dst =
                unsafe { MmapMut::map_mut(&dst) }.inspect_err(log_err!(
                    "memory mapping the snapshot file for the accountsdb file",
                ))?;
            dst.copy_from_slice(mmap);
            // we move the flushing to separate thread to avoid blocking
            std::thread::spawn(move || {
                dst.flush().inspect_err(log_err!(
                    "flushing accounts.db file after mmap copy"
                ))
            });
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

#[cfg(test)]
impl SnapshotEngine {
    pub fn snapshot_exists(&self, slot: u64) -> bool {
        let spath = SnapSlot(slot).as_path(Self::snapshots_dir(&self.dbpath));
        let snapshots = self.snapshots.lock(); // free lock

        // paths to snapshots are strictly ordered, so we can b-search
        snapshots.binary_search(&spath).is_ok()
    }
}
