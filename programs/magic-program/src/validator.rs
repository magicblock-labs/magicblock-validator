//! Access to the validator (engine) authority for builtin programs.
//!
//! While a transaction executes, the engine publishes its authority's pubkey
//! on the current thread through [`nucleus::tls::AUTHORITY`]. Builtin programs
//! use only this public identity; they do not hold the validator's keypair.

use solana_pubkey::Pubkey;

/// Returns the engine authority pubkey for the current execution thread.
///
/// Populated by the engine's executor/simulator before every execution; reads
/// back the default pubkey on threads where no execution is in flight.
pub fn authority() -> Pubkey {
    nucleus::tls::AUTHORITY.get()
}
