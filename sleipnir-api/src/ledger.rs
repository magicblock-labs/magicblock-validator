use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::exit,
};

use fd_lock::{RwLock, RwLockWriteGuard};
use log::*;
use sleipnir_ledger::Ledger;

use crate::errors::{ApiError, ApiResult};

pub(crate) fn init(ledger_path: PathBuf, reset: bool) -> ApiResult<Ledger> {
    if reset {
        remove_directory_contents_if_exists(ledger_path.as_path()).map_err(
            |err| {
                error!(
                    "Error: Unable to remove {}: {}",
                    ledger_path.display(),
                    err
                );
                ApiError::UnableToCleanLedgerDirectory(
                    ledger_path.display().to_string(),
                )
            },
        )?;
    }

    fs::create_dir_all(&ledger_path)?;

    Ok(Ledger::open(ledger_path.as_path())?)
}

fn remove_directory_contents_if_exists(
    dir: &Path,
) -> Result<(), std::io::Error> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.metadata()?.is_dir() {
            fs::remove_dir_all(entry.path())?
        } else {
            fs::remove_file(entry.path())?
        }
    }
    Ok(())
}

pub fn ledger_lockfile(ledger_path: &Path) -> RwLock<File> {
    let lockfile = ledger_path.join("ledger.lock");
    fd_lock::RwLock::new(
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(lockfile)
            .unwrap(),
    )
}

pub fn lock_ledger<'lock>(
    ledger_path: &Path,
    ledger_lockfile: &'lock mut RwLock<File>,
) -> RwLockWriteGuard<'lock, File> {
    ledger_lockfile.try_write().unwrap_or_else(|_| {
        println!(
            "Error: Unable to lock {} directory. Check if another validator is running",
            ledger_path.display()
        );
        exit(1);
    })
}
