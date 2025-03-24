use std::{
    collections::VecDeque,
    fs,
    fs::File,
    io,
    io::Write,
    path::{Path, PathBuf},
};

use log::{info, warn};
use parking_lot::Mutex;
use reflink::reflink;

use crate::{error::AccountsDbError, log_err, AdbResult};

pub struct SnapshotEngine {
    /// directory path where database files are kept
    dbpath: PathBuf,
    /// snapshotting function
    snapfn: fn(&Path, &Path) -> io::Result<()>,
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
        let snapfn = if is_cow_supported {
            info!("Host file system supports CoW, will use reflinking (fast)");
            reflink_dir
        } else {
            info!(
                "Host file system doesn't support CoW, will use regular (slow) file copy"
            );
            rcopy_dir
        };
        let snapshots = Self::read_snapshots(&dbpath, max_count)?.into();

        Ok(Box::new(Self {
            dbpath,
            snapfn,
            snapshots,
            max_count,
        }))
    }

    /// Take snapshot of database directory, this operation
    /// assumes that no writers are currently active
    pub(crate) fn snapshot(&self, slot: u64) -> AdbResult<()> {
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

        (self.snapfn)(&self.dbpath, &snapout)?;
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
            // if we have snapshot older than slot, use it
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
            // if this operation fails (which is unlikely), then it most likely failed to path
            // being invalid, which is fine by us, since we wanted to remove it anyway
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
        let result = reflink(&tmp, &tmpsnap).is_ok();
        if tmp.exists() {
            fs::remove_file(tmp)?;
        }
        // if we failed to create the file then the below operation will fail,
        // but since we wanted to remove it anyway, just ignore the error
        let _ = fs::remove_file(tmpsnap);
        Ok(result)
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

#[inline(always)]
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
            copyfile(&src, &dst)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn copyfile(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let src = File::open(src)?;
    let dst = File::create(dst)?;
    let mut offset = 0_i64;
    let file_len = src.metadata()?.len() as i64;

    while offset < file_len {
        let mut size = file_len - offset;
        let result = unsafe {
            libc::sendfile(
                src.as_raw_fd(),
                dst.as_raw_fd(),
                offset,
                &mut size as *mut i64,
                std::ptr::null_mut(),
                0,
            )
        };

        if result == -1 {
            return Err(io::Error::last_os_error());
        }

        offset += size;
    }

    debug_assert_eq!(
        offset as u64, file_len as u64,
        "entire file should have been copied over"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn copyfile(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let src = File::open(src)?;
    let dst = File::create(dst)?;
    let mut offset = 0;
    let size = src.metadata()?.len() as usize;

    while offset < size {
        let to_send = size - offset;
        let result = unsafe {
            libc::sendfile(
                dst.as_raw_fd(),
                src.as_raw_fd(),
                &mut offset as *mut _ as *mut libc::off_t,
                to_send,
            )
        };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        offset += result as usize;
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
