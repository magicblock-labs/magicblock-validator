use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::exit,
};

use fd_lock::{RwLock, RwLockWriteGuard};
use log::*;
use sleipnir_ledger::Ledger;
use solana_sdk::{signature::Keypair, signer::EncodableKey};

use crate::{
    errors::{ApiError, ApiResult},
    utils::fs::remove_directory_contents_if_exists,
};

// -----------------
// Init
// -----------------
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

// -----------------
// Lockfile
// -----------------
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

// -----------------
// Faucet
// -----------------
fn faucet_keypair_path(ledger_path: &Path) -> ApiResult<PathBuf> {
    let parent = ledger_parent_dir(ledger_path)?;
    Ok(parent.join("faucet-keypair.json"))
}

pub(crate) fn read_faucet_keypair_from_ledger(
    ledger_path: &Path,
) -> ApiResult<Keypair> {
    let keypair_path = faucet_keypair_path(ledger_path)?;
    if fs::exists(keypair_path.as_path()).unwrap_or(false) {
        let keypair =
            Keypair::read_from_file(keypair_path.as_path()).map_err(|err| {
                ApiError::LedgerInvalidFaucetKeypair(
                    keypair_path.display().to_string(),
                    err.to_string(),
                )
            })?;
        Ok(keypair)
    } else {
        Err(ApiError::LedgerIsMissingFaucetKeypair(
            keypair_path.display().to_string(),
        ))
    }
}

pub(crate) fn write_faucet_keypair_to_ledger(
    ledger_path: &Path,
    keypair: &Keypair,
) -> ApiResult<()> {
    let keypair_path = faucet_keypair_path(ledger_path)?;
    keypair
        .write_to_file(keypair_path.as_path())
        .map_err(|err| {
            ApiError::LedgerCouldNotWriteFaucetKeypair(
                keypair_path.display().to_string(),
                err.to_string(),
            )
        })?;
    Ok(())
}

// -----------------
// Ledger Directories
// -----------------
pub(crate) fn ledger_parent_dir(ledger_path: &Path) -> ApiResult<PathBuf> {
    let parent = ledger_path.parent().ok_or_else(|| {
        ApiError::LedgerPathIsMissingParent(
            ledger_path.to_path_buf().display().to_string(),
        )
    })?;
    Ok(parent.to_path_buf())
}
