use nix::sys::sendfile::sendfile;
use parking_lot::Mutex;
use reflink::reflink;
use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::{fs, io};

use crate::error::AdbError;
use crate::AdbResult;

pub struct SnapshotEngine {
    /// directory path where database files are kept
    dbpath: PathBuf,
    /// snapshotting function
    snapfn: fn(&Path, &Path) -> io::Result<()>,
    /// List of existing snapshots
    /// Note: as it's locked only when slot is incremented
    /// this is is basically a contention free Mutex
    /// we use it for the convenience of interior mutability
    snapshots: Mutex<VecDeque<PathBuf>>,
    /// max number of snapshots to keep alive
    maxcount: u16,
}

impl SnapshotEngine {
    pub(crate) fn new(dbpath: PathBuf, maxcount: u16) -> AdbResult<Box<Self>> {
        let snapshots = Mutex::new(VecDeque::with_capacity(maxcount as usize));
        let is_cow_supported =
            inspecterr!(Self::supports_cow(&dbpath), "cow support check");
        let snapfn = if is_cow_supported {
            reflink_dir
        } else {
            rcopy_dir
        };
        let this = Self {
            dbpath,
            snapfn,
            snapshots,
            maxcount,
        };
        inspecterr!(this.read_snapshots(), "reading existing snapshots");

        Ok(Box::new(this))
    }

    /// Take snapshot of database directory, this operation
    /// assumes that no writers are currently active
    pub(crate) fn snapshot(&self, slot: u64) -> AdbResult<()> {
        let slot = SnapSlot(slot);
        let mut snapshots = self.snapshots.lock(); // free lock
        if snapshots.len() == self.maxcount as usize {
            if let Some(old) = snapshots.pop_front() {
                fs::remove_dir_all(&old)?;
            }
        }
        let snapout = slot.as_path(self.snapshots_dir());

        (self.snapfn)(&self.dbpath, &snapout)?;
        snapshots.push_back(snapout);
        Ok(())
    }

    /// Try to rollback to snapshot which is the most recent one before given slot
    /// NOTE: In case of success, this deletes the primary database, careful!
    pub(crate) fn try_switch_to_snapshot(
        &self,
        mut slot: u64,
    ) -> AdbResult<u64> {
        let mut spath = SnapSlot(slot).as_path(self.snapshots_dir());
        let mut snapshots = self.snapshots.lock(); // free lock

        // paths to snapshots are strictly ordered, so we can b-search
        let index = match snapshots.binary_search(&spath) {
            Ok(i) => i,
            // if we have snapshot older than slot, use it
            Err(i) if i != 0 => i - 1,
            // otherwise we don't have any snapshot before the given slot
            Err(_) => return Err(AdbError::SnapshotMissing(slot)),
        };

        spath = snapshots.remove(index).unwrap(); // infallible

        // infallible, all entries in snapshots are created with SnapSlot naming conventions
        slot = SnapSlot::try_from_path(&spath).unwrap().0;

        // we perform database swap, thus removing
        // latest state and rolling back to snapshot
        fs::remove_dir_all(&self.dbpath)?;
        fs::rename(&spath, &self.dbpath)?;

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
        let result = reflink(&tmp, &tmpsnap).is_ok();
        fs::remove_file(tmp)?;
        let _ = fs::remove_file(tmpsnap);
        Ok(result)
    }

    /// Reads the list of snapshots directories from disk, this
    /// is necessary to restore last state after restart
    fn read_snapshots(&self) -> io::Result<()> {
        let snapdir = self.snapshots_dir();
        if !snapdir.exists() {
            fs::create_dir(&snapdir)?;
        }
        let mut snapshots = self.snapshots.lock();
        for entry in fs::read_dir(snapdir)? {
            let entry = entry?;
            let snap = entry.path();
            if snap.is_dir() && SnapSlot::try_from_path(&snap).is_some() {
                snapshots.push_back(snap);
            }
        }
        // sorting is required for correct ordering (slot-wise) of snapshots
        snapshots.make_contiguous().sort();

        while snapshots.len() > self.maxcount as usize {
            snapshots.pop_front();
        }
        Ok(())
    }

    fn snapshots_dir(&self) -> PathBuf {
        let mut parent = self.dbpath.clone();
        parent.pop();
        parent
    }
}

fn reflink_dir(src: &Path, dst: &Path) -> io::Result<()> {
    reflink::reflink(src, dst)
}

fn rcopy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src = entry.path();
        let dst = dst.join(entry.file_name());

        if src.is_dir() {
            rcopy_dir(&src, &dst)?;
        } else {
            let src = File::open(src)?;
            let dst = File::create(dst)?;
            sendfile(dst, src, 0, None, None, None).0?;
        }
    }
    Ok(())
}

#[cfg(test)]
impl SnapshotEngine {
    pub fn snapshot_exists(&self, slot: u64) -> bool {
        let spath = SnapSlot(slot).as_path(self.snapshots_dir());
        let snapshots = self.snapshots.lock(); // free lock

        // paths to snapshots are strictly ordered, so we can b-search
        snapshots.binary_search(&spath).is_ok()
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

    fn as_path(&self, ppath: PathBuf) -> PathBuf {
        // enforce strict alphanumberic ordering by introducing extra padding
        ppath.join(format!("snapshot-{:0>9}", self.0))
    }
}

#[cfg(any(feature = "dev-tools", test))]
/// Convenience Drop impl for autocleanup during tests
impl Drop for SnapshotEngine {
    fn drop(&mut self) {
        self.dbpath.pop();
        let _ = fs::remove_dir_all(&self.dbpath);
    }
}
