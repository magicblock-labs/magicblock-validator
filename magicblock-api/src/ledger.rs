use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::exit,
};

use fd_lock::{RwLock, RwLockWriteGuard};
use log::*;
use magicblock_config::config::LedgerConfig;
use magicblock_ledger::{Ledger, BLOCKSTORE_DIRECTORY_ROCKS_LEVEL};
use solana_sdk::{clock::Slot, signature::Keypair, signer::EncodableKey};

use crate::errors::{ApiError, ApiResult};

// -----------------
// Init
// -----------------
pub(crate) fn init(
    path: &Path,
    config: &LedgerConfig,
) -> ApiResult<(Ledger, Slot)> {
    if config.reset {
        remove_ledger_directory_if_exists(path).map_err(|err| {
            error!("Unable to remove the ledger {}: {}", path.display(), err);
            ApiError::UnableToCleanLedgerDirectory(path.display().to_string())
        })?;
    };
    let ledger = Ledger::open(path)?;
    let slot = if config.reset {
        // If the ledger was reset, then we use whatever
        // current slot is available in the AccountsDB
        Slot::MAX
    } else {
        ledger.get_max_blockhash().map(|(slot, _)| slot)?
    };
    Ok((ledger, slot))
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
    if fs::exists(keypair_path.as_path()).unwrap_or_default() {
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
// Validator Keypair
// -----------------
pub(crate) fn validator_keypair_path(ledger_path: &Path) -> ApiResult<PathBuf> {
    let parent = ledger_parent_dir(ledger_path)?;
    Ok(parent.join("validator-keypair.json"))
}

pub(crate) fn read_validator_keypair_from_ledger(
    ledger_path: &Path,
) -> ApiResult<Keypair> {
    let keypair_path = validator_keypair_path(ledger_path)?;
    if fs::exists(keypair_path.as_path()).unwrap_or_default() {
        let keypair =
            Keypair::read_from_file(keypair_path.as_path()).map_err(|err| {
                ApiError::LedgerInvalidValidatorKeypair(
                    keypair_path.display().to_string(),
                    err.to_string(),
                )
            })?;
        Ok(keypair)
    } else {
        Err(ApiError::LedgerIsMissingValidatorKeypair(
            keypair_path.display().to_string(),
        ))
    }
}

pub(crate) fn write_validator_keypair_to_ledger(
    ledger_path: &Path,
    keypair: &Keypair,
) -> ApiResult<()> {
    let keypair_path = validator_keypair_path(ledger_path)?;
    keypair
        .write_to_file(keypair_path.as_path())
        .map_err(|err| {
            ApiError::LedgerCouldNotWriteValidatorKeypair(
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

fn remove_ledger_directory_if_exists(storage_path: &Path) -> ApiResult<()> {
    // see Ledger::do_open for this hardcoded path
    let ledger_path = storage_path.join(BLOCKSTORE_DIRECTORY_ROCKS_LEVEL);
    let keypair_path = validator_keypair_path(&ledger_path)?;
    if ledger_path.exists() {
        fs::remove_dir_all(ledger_path)?;
    }
    if keypair_path.exists() {
        fs::remove_file(keypair_path)?;
    }

    Ok(())
}
