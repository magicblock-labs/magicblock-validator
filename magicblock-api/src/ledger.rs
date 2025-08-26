use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::exit,
};

use fd_lock::{RwLock, RwLockWriteGuard};
use log::*;
use magicblock_accounts_db::ACCOUNTSDB_DIR;
use magicblock_config::{LedgerConfig, LedgerResumeStrategy};
use magicblock_ledger::Ledger;
use solana_sdk::{clock::Slot, signature::Keypair, signer::EncodableKey};

use crate::errors::{ApiError, ApiResult};

// -----------------
// Init
// -----------------
pub(crate) fn init(
    ledger_path: &Path,
    ledger_config: &LedgerConfig,
) -> ApiResult<(Ledger, Slot)> {
    let resume_strategy = &ledger_config.resume_strategy();
    let starting_slot = match resume_strategy {
        LedgerResumeStrategy::Resume { .. } => {
            let previous_ledger = Ledger::open(ledger_path)?;
            previous_ledger.get_max_blockhash().map(|(slot, _)| slot)?
        }
        LedgerResumeStrategy::Reset { slot, .. } => *slot,
    };

    if resume_strategy.is_removing_ledger() {
        remove_ledger_directory_if_exists(ledger_path, resume_strategy)
            .map_err(|err| {
                error!(
                    "Error: Unable to remove {}: {}",
                    ledger_path.display(),
                    err
                );
                ApiError::UnableToCleanLedgerDirectory(
                    ledger_path.display().to_string(),
                )
            })?;
    }

    Ok((Ledger::open(ledger_path)?, starting_slot))
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
fn validator_keypair_path(ledger_path: &Path) -> ApiResult<PathBuf> {
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

fn remove_ledger_directory_if_exists(
    dir: &Path,
    resume_strategy: &LedgerResumeStrategy,
) -> Result<(), std::io::Error> {
    if !dir.exists() {
        return Ok(());
    }
    debug!("Strategy: {:?}", resume_strategy);
    for entry in fs::read_dir(dir)? {
        let entry = entry?;

        if entry.file_name() == ACCOUNTSDB_DIR
            && !resume_strategy.is_removing_accountsdb()
        {
            // The entry is accounts db
            debug!("Skipping accounts db: {}", entry.path().display());
            continue;
        } else if let Ok(validator_keypair_path) = validator_keypair_path(dir) {
            // The entry is the validator keypair
            if resume_strategy.is_removing_ledger()
                && validator_keypair_path
                    .file_name()
                    .map(|key_path| key_path == entry.file_name())
                    .unwrap_or_default()
            {
                debug!(
                    "Skipping validator keypair: {}",
                    entry.path().display()
                );
                continue;
            }
        }

        debug!("Removing ledger entry: {}", entry.path().display());
        if entry.metadata()?.is_dir() {
            fs::remove_dir_all(entry.path())?
        } else {
            fs::remove_file(entry.path())?
        }
    }
    Ok(())
}
