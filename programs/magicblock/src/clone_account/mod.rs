//! Account cloning instructions for the ephemeral validator.
//!
//! # Overview
//!
//! Accounts are cloned from the remote chain via direct encoding in transactions.
//! Large accounts (>63KB) are split across multiple sequential transactions.
//!
//! # Flow for Regular Accounts
//!
//! 1. Small (<63KB): Single `CloneAccount` instruction
//! 2. Large (>=63KB): `CloneAccountInit` → `CloneAccountContinue`* → (complete)
//!
//! # Flow for Program Accounts
//!
//! Programs require a buffer-based approach to handle loader-specific logic:
//!
//! 1. Clone ELF data to a buffer account (dummy account owned by system program)
//! 2. `FinalizeProgramFromBuffer` or `FinalizeV1ProgramFromBuffer` creates the
//!    actual program accounts with proper loader headers
//! 3. For V4: `LoaderV4::Deploy` is called, then `SetProgramAuthority`
//!
//! # Lamports Accounting
//!
//! All instructions track lamports delta and adjust the validator authority account
//! to maintain balanced transactions. The validator authority is funded at startup.

mod common;
mod process_cleanup;
mod process_clone;
mod process_clone_continue;
mod process_clone_init;
mod process_finalize_buffer;
mod process_finalize_v1_buffer;
mod process_set_authority;

use std::collections::HashSet;

use lazy_static::lazy_static;
use parking_lot::RwLock;
use solana_pubkey::Pubkey;

pub(crate) use common::*;
pub(crate) use process_cleanup::process_cleanup_partial_clone;
pub(crate) use process_clone::process_clone_account;
pub(crate) use process_clone_continue::process_clone_account_continue;
pub(crate) use process_clone_init::process_clone_account_init;
pub(crate) use process_finalize_buffer::process_finalize_program_from_buffer;
pub(crate) use process_finalize_v1_buffer::process_finalize_v1_program_from_buffer;
pub(crate) use process_set_authority::process_set_program_authority;

lazy_static! {
    /// Tracks in-progress multi-transaction clones.
    /// - `CloneAccountInit` adds the pubkey
    /// - `CloneAccountContinue(is_last=true)` removes it
    /// - `CleanupPartialClone` removes it on failure
    static ref PENDING_CLONES: RwLock<HashSet<Pubkey>> = RwLock::new(HashSet::new());
}

pub fn is_pending_clone(pubkey: &Pubkey) -> bool {
    PENDING_CLONES.read().contains(pubkey)
}

pub fn add_pending_clone(pubkey: Pubkey) -> bool {
    PENDING_CLONES.write().insert(pubkey)
}

pub fn remove_pending_clone(pubkey: &Pubkey) -> bool {
    PENDING_CLONES.write().remove(pubkey)
}
