//! Access to the validator (engine) authority for builtin programs.
//!
//! While a transaction executes, the engine publishes its authority's pubkey
//! on the current thread through [`nucleus::tls::AUTHORITY`]. The configured
//! signer is also retained so scheduling can pre-sign the later
//! `ScheduledCommitSent` transaction and expose its signature to callers.

use std::sync::{Arc, OnceLock};

use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

static VALIDATOR_AUTHORITY: OnceLock<Arc<Keypair>> = OnceLock::new();

/// Returns the engine authority pubkey for the current execution thread.
///
/// Populated by the engine's executor/simulator before every execution; reads
/// back the default pubkey on threads where no execution is in flight.
pub fn authority() -> Pubkey {
    nucleus::tls::AUTHORITY.get()
}

pub fn validator_authority() -> Arc<Keypair> {
    VALIDATOR_AUTHORITY.wait().clone()
}

/// Sets the thread-local engine authority.
///
/// In production the engine runtime is the sole writer of this value; this is
/// used only by the test/dev harness to establish a known authority on the
/// current thread.
pub fn set_authority(pubkey: Pubkey) {
    nucleus::tls::AUTHORITY.set(pubkey);
}

/// Ensures a non-default authority is set on the current thread, generating a
/// fresh one if needed, and returns it. Test/dev harness helper.
pub fn generate_validator_authority_if_needed() -> Pubkey {
    let authority = VALIDATOR_AUTHORITY
        .get_or_init(|| Arc::new(Keypair::new()))
        .pubkey();
    let current = nucleus::tls::AUTHORITY.get();
    if current == Pubkey::default() {
        nucleus::tls::AUTHORITY.set(authority);
        authority
    } else {
        current
    }
}

pub fn init_validator_authority(keypair: impl Into<Arc<Keypair>>) {
    let authority = VALIDATOR_AUTHORITY.get_or_init(|| keypair.into()).pubkey();
    set_authority(authority);
}

/// Test-only alias for [`authority`], kept so existing tests read unchanged.
#[cfg(test)]
pub fn validator_authority_id() -> Pubkey {
    authority()
}

#[cfg(not(test))]
pub fn validator_authority_id() -> Pubkey {
    validator_authority().pubkey()
}
