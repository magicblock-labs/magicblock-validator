//! Validated delegation images shared by acquisition and stream matching.

use dlp_api::state::DelegationRecord;
use solana_account::{AccountBuilder, AccountMode};
use solana_pubkey::Pubkey;

/// Decodes the shared on-chain delegation-record representation.
pub(crate) fn record(data: &[u8]) -> Option<&DelegationRecord> {
    DelegationRecord::try_from_bytes_with_discriminator(data).ok()
}

/// Accepts this Engine's authority or the default authority used for confined accounts.
pub(crate) fn belongs_to(record: &DelegationRecord, authority: Pubkey) -> bool {
    record.authority == authority || record.authority == Pubkey::default()
}

/// Restores the recorded owning program and slot, then selects delegated or confined mode.
/// Requires a validated record whose authority is local or permits authority-free confinement.
pub(crate) fn account(account: AccountBuilder, record: &DelegationRecord) -> AccountBuilder {
    let account = account.owner(record.owner).slot(record.delegation_slot);
    // No authority can commit a confined account back to the base chain.
    if record.authority == Pubkey::default() {
        account.lamports(0).mode(AccountMode::Magic)
    } else {
        account.mode(AccountMode::Delegated)
    }
}

/// Account image restored using a validated delegation record.
pub(crate) struct Delegation {
    pub(crate) pubkey: Pubkey,
    /// Account with original owner, `Delegated` or confined `Magic` mode, and delegation slot.
    pub(crate) account: AccountBuilder,
    /// Full record, including appended post-delegation actions.
    pub(crate) record: Vec<u8>,
    /// Original owning program from the validated delegation record.
    pub(crate) source_program: Pubkey,
}
