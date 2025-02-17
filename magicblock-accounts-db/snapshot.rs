use nix::sys::sendfile::sendfile;
use parking_lot::Mutex;
use reflink::reflink;
use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::{fs, io};

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
        let is_cow_supported = inspecterr!(Self::supports_cow(&dbpath), "cow support check");
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

    /// Retrieves the path to snapshot at given slot if it exists
    pub(crate) fn path_to_snapshot_at(&self, slot: u64) -> Option<PathBuf> {
        let slot = SnapSlot(slot).as_path(self.snapshots_dir());
        let snapshots = self.snapshots.lock();
        let index = snapshots.binary_search(&slot).ok()?;
        snapshots.get(index).cloned()
    }

    /// Makes given snapshot primary, effectively rebasing onto it
    pub(crate) fn switch_to_snapshot(&mut self, dbpath: PathBuf) {
        let mut snapshots = self.snapshots.lock();
        let Some(i) = snapshots.iter().position(|p| p == &dbpath) else {
            return;
        };
        snapshots.remove(i);
        self.dbpath = dbpath;
    }

    /// Perform test to find out whether file system
    /// supports COW operations (btrfs, xfs, zfs, apfs)
    fn supports_cow(dir: &Path) -> io::Result<bool> {
        let tmp = dir.join("__tempfile.fs");
        let mut file = File::create(&tmp)?;
        file.set_len(64)?;
        file.write_all(&[42; 64])?;
        file.flush()?;
        let tmpsnap = dir.join("__tempfile_snap.fs");
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
            if snap.is_dir() {
                snapshots.push_back(snap);
            }
        }
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
    todo!()
}

#[derive(Eq, PartialEq, PartialOrd, Ord)]
struct SnapSlot(u64);

impl SnapSlot {
    /// parse snapshot path to extract slot number
    #[allow(unused)]
    fn try_from_path(path: &Path) -> Option<Self> {
        path.to_str()
            .and_then(|s| s.split('-').nth(1))
            .and_then(|s| s.parse::<u64>().ok())
            .map(Self)
    }

    fn as_path(&self, ppath: PathBuf) -> PathBuf {
        // enforce strict alphanumberic ordering by introducing extra padding
        ppath.join(format!("snapshot-{:0>9}", self.0))
    }
}
