use std::sync::{
    atomic::{AtomicBool, Ordering},
    RwLock,
};

use lazy_static::lazy_static;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

lazy_static! {
    static ref VALIDATOR_AUTHORITY: RwLock<Option<Keypair>> = RwLock::new(None);

    /// Flag to indicate if the validator is starting up which includes
    /// processing the ledger.
    /// Certain transactions behave slightly different during that phase
    /// especially those that interact with main chain like account mutations
    /// and scheduled commits.
    static ref STARTING_UP: AtomicBool = AtomicBool::new(
        #[cfg(not(test))]
        true,
        // our unit tests assume the validator is already running
        #[cfg(test)]
        false,
    );
}

pub fn validator_authority() -> Keypair {
    VALIDATOR_AUTHORITY
        .read()
        .expect("RwLock VALIDATOR_AUTHORITY poisoned")
        .as_ref()
        .expect("Validator authority needs to be set on startup")
        .insecure_clone()
}

pub fn validator_authority_id() -> Pubkey {
    VALIDATOR_AUTHORITY
        .read()
        .expect("RwLock VALIDATOR_AUTHORITY poisoned")
        .as_ref()
        .map(|x| x.pubkey())
        .expect("Validator authority needs to be set on startup")
}

pub fn init_validator_authority(keypair: Keypair) {
    let mut validator_authority_lock = VALIDATOR_AUTHORITY
        .write()
        .expect("RwLock VALIDATOR_AUTHORITY poisoned");
    if let Some(validator_authority) = validator_authority_lock.as_ref() {
        panic!("Validator authority can only be set once, but was set before to '{}'", validator_authority.pubkey());
    }
    validator_authority_lock.replace(keypair);
}

pub fn init_validator_authority_if_needed(keypair: Keypair) {
    let mut validator_authority_lock = VALIDATOR_AUTHORITY
        .write()
        .expect("RwLock VALIDATOR_AUTHORITY poisoned");
    if validator_authority_lock.as_ref().is_some() {
        return;
    }
    validator_authority_lock.replace(keypair);
}

pub fn generate_validator_authority_if_needed() {
    let mut validator_authority_lock = VALIDATOR_AUTHORITY
        .write()
        .expect("RwLock VALIDATOR_AUTHORITY poisoned");
    if validator_authority_lock.as_ref().is_some() {
        return;
    }
    validator_authority_lock.replace(Keypair::new());
}

/// Returns `true` if the validator is starting up which is the initial
/// state.
pub fn is_starting_up() -> bool {
    STARTING_UP.load(Ordering::Relaxed)
}

/// Ensures that the flag indicating if the validator started up is flipped
/// to `false`.
/// This version does not check if the validator was already started up and
/// thus should only be used in tests.
pub fn ensure_started_up() {
    STARTING_UP.store(false, Ordering::Relaxed);
}

/// Needs to be called after the validator is done starting up, i.e.
/// the ledger has been processed.
/// This version ensures that the validator hadn't started before and
/// should be used in prod code to avoid logic errors.
pub fn finished_starting_up() {
    let was_starting_up =
        STARTING_UP.swap(false, std::sync::atomic::Ordering::Relaxed);
    assert!(
        was_starting_up,
        "validator::finished_starting_up should only be called once"
    );
}
