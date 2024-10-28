use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::utils;

pub const ACCOUNTS_RUN_DIR: &str = "run";
pub const ACCOUNTS_SNAPSHOT_DIR: &str = "snapshot";

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
    if (!run_path.is_dir()) || (!snapshot_path.is_dir()) {
        // If the "run/" or "snapshot" sub directories do not exist, the directory
        // may be from an older version for which the appendvec files are at
        // this directory. Clean up them first.
        // This will be done only once when transitioning from an old image
        // without run directory to this new version using run and snapshot directories.
        // The run/ content cleanup will be done at a later point.
        // The snapshot/ content persists across the process boot, and will be purged
        // by the account_background_service.
        utils::fs::remove_directory_contents_if_exists(account_dir.as_ref())?;

        fs::create_dir_all(&run_path)?;
        fs::create_dir_all(&snapshot_path)?;
    }

    Ok((run_path, snapshot_path))
}
