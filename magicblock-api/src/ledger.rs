use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::exit,
};

use fd_lock::{RwLock, RwLockWriteGuard};
use magicblock_config::config::LedgerConfig;
use magicblock_ledger::{Ledger, BLOCKSTORE_DIRECTORY_ROCKS_LEVEL};
use solana_keypair::Keypair;
use solana_program::clock::Slot;
use solana_signer::EncodableKey;
use tracing::*;

use crate::errors::{ApiError, ApiResult};

/// Represents the initialization state of the ledger
#[derive(Debug, Clone, Copy)]
pub enum LedgerInitState {
    /// Ledger was reset/fresh start - no existing blocks
    Fresh,
    /// Resuming from an existing slot
    Resuming { last_slot: Slot },
}

impl LedgerInitState {
    /// Returns the slot to use for AccountsDB initialization
    pub fn slot_for_accountsdb(&self) -> Slot {
        match self {
            LedgerInitState::Fresh => Slot::MAX,
            LedgerInitState::Resuming { last_slot } => *last_slot,
        }
    }
}

impl std::fmt::Display for LedgerInitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerInitState::Fresh => write!(f, "fresh start (no existing blocks)"),
            LedgerInitState::Resuming { last_slot } => {
                write!(f, "resuming from slot {}", last_slot)
            }
        }
    }
}

// -----------------
// Init
// -----------------
pub(crate) fn init(
    path: &Path,
    config: &LedgerConfig,
) -> ApiResult<(Ledger, LedgerInitState)> {
    if config.reset {
        remove_ledger_directory_if_exists(path).map_err(|err| {
            error!(error = ?err, path = %path.display(), "Unable to remove ledger");
            ApiError::UnableToCleanLedgerDirectory(path.display().to_string())
        })?;
    };
    let ledger = Ledger::open(path)?;
    let init_state = if config.reset {
        LedgerInitState::Fresh
    } else {
        let slot = ledger.get_max_blockhash().map(|(slot, _)| slot)?;
        LedgerInitState::Resuming { last_slot: slot }
    };
    Ok((ledger, init_state))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ledger_init_state_fresh_display() {
        let state = LedgerInitState::Fresh;
        assert_eq!(state.to_string(), "fresh start (no existing blocks)");
        assert_eq!(state.slot_for_accountsdb(), Slot::MAX);
    }

    #[test]
    fn test_ledger_init_state_resuming_display() {
        let state = LedgerInitState::Resuming { last_slot: 42 };
        assert_eq!(state.to_string(), "resuming from slot 42");
        assert_eq!(state.slot_for_accountsdb(), 42);
    }
}
