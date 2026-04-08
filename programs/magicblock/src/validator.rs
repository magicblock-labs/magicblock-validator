use std::sync::RwLock;

use lazy_static::lazy_static;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

lazy_static! {
    static ref VALIDATOR_AUTHORITY: RwLock<Option<Keypair>> = RwLock::new(None);
    static ref VALIDATOR_AUTHORITY_OVERRIDE: RwLock<Option<Pubkey>> =
        RwLock::new(None);
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

pub fn set_validator_authority_override(pubkey: Pubkey) {
    let mut lock = VALIDATOR_AUTHORITY_OVERRIDE
        .write()
        .expect("RwLock VALIDATOR_AUTHORITY_OVERRIDE poisoned");
    lock.replace(pubkey);
}

pub fn unset_validator_authority_override() {
    let mut lock = VALIDATOR_AUTHORITY_OVERRIDE
        .write()
        .expect("RwLock VALIDATOR_AUTHORITY_OVERRIDE poisoned");
    *lock = None;
}

pub fn validator_authority_override() -> Option<Pubkey> {
    *VALIDATOR_AUTHORITY_OVERRIDE
        .read()
        .expect("RwLock VALIDATOR_AUTHORITY_OVERRIDE poisoned")
}

pub fn effective_validator_authority_id() -> Pubkey {
    validator_authority_override().unwrap_or_else(validator_authority_id)
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
