use std::{
    fs,
    path::{Path, PathBuf},
};

use log::*;
use magicblock_accounts_db::{ACCOUNTS_RUN_DIR, ACCOUNTS_SNAPSHOT_DIR};
use magicblock_bank::bank::Bank;
use magicblock_metrics::metrics;

use crate::utils;

/// To allow generating a bank snapshot directory with full state information,
/// we need to hardlink account appendvec files from the runtime operation
/// directory to a snapshot hardlink directory.
/// This is to create the run/ and snapshot sub directories for an account_path
/// provided by the user.
/// These two sub directories are on the same file system partition
/// to allow hard-linking.
pub fn create_accounts_run_and_snapshot_dirs(
    account_dir: impl AsRef<Path>,
) -> std::io::Result<(PathBuf, PathBuf)> {
    let run_path = account_dir.as_ref().join(ACCOUNTS_RUN_DIR);
    let snapshot_path = account_dir.as_ref().join(ACCOUNTS_SNAPSHOT_DIR);
    if !run_path.is_dir() {
        utils::fs::remove_directory_contents_if_exists(account_dir.as_ref())?;
        fs::create_dir_all(&run_path)?;
    }
    if snapshot_path.is_dir() {
        fs::create_dir_all(&snapshot_path)?;
        fs::create_dir_all(&run_path)?;
    }

    Ok((run_path, snapshot_path))
}

pub fn flush_accounts(bank: &Bank) {
    metrics::observe_flush_accounts_time(|| {
        trace!("Flushing accounts");
        match bank.flush_accounts_cache() {
            Ok(storage_entries_removed) => {
                if storage_entries_removed > 0 {
                    debug!("Flushed accounts cache and removed {} older storage entries from disk",
                        storage_entries_removed);
                }
            }
            Err(err) => {
                error!("Encountered error when flushing accounts: {:?}", err)
            }
        }
    });
}
