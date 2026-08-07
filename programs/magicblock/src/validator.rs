//! Access to the validator (engine) authority for builtin programs.
//!
//! The validator no longer owns a keypair of its own: the engine's signer is
//! the single validator identity. While a transaction executes, the engine
//! publishes that identity's pubkey on the current thread through
//! [`nucleus::tls::AUTHORITY`], and builtins read it from there. Signing is the
//! engine's responsibility and never happens inside a builtin.

use solana_pubkey::Pubkey;

/// Returns the engine authority pubkey for the current execution thread.
///
/// Populated by the engine's executor/simulator before every execution; reads
/// back the default pubkey on threads where no execution is in flight.
pub fn authority() -> Pubkey {
    nucleus::tls::AUTHORITY.get()
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
    let current = nucleus::tls::AUTHORITY.get();
    if current != Pubkey::default() {
        return current;
    }
    let pubkey = Pubkey::new_unique();
    nucleus::tls::AUTHORITY.set(pubkey);
    pubkey
}

/// Test-only shim mirroring the former keypair-based initializer: stores only
/// the keypair's pubkey as the thread-local authority (the keypair itself is
/// discarded — the engine owns the signer).
#[cfg(test)]
pub fn init_validator_authority(keypair: solana_keypair::Keypair) {
    use solana_signer::Signer;
    set_authority(keypair.pubkey());
}

/// Test-only alias for [`authority`], kept so existing tests read unchanged.
#[cfg(test)]
pub fn validator_authority_id() -> Pubkey {
    authority()
}
