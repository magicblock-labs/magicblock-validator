//! Cleanup for failed multi-transaction clones.

use std::collections::HashSet;

use solana_account::ReadableAccount;
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionContext;

use super::{
    adjust_authority_lamports, close_buffer_account, is_pending_clone,
    remove_pending_clone, validate_and_get_index, validate_authority,
};
use crate::errors::MagicBlockProgramError;

/// Cleans up a failed multi-transaction clone.
///
/// Called when any transaction in the clone sequence fails.
/// Removes from `PENDING_CLONES` and resets the account to default state,
/// returning its lamports to the validator authority.
pub(crate) fn process_cleanup_partial_clone(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    pubkey: Pubkey,
) -> Result<(), InstructionError> {
    validate_authority(signers, invoke_context)?;

    // Safety check: only cleanup accounts that are actually in a pending clone state.
    // This prevents accidental deletion of valid accounts if cleanup is called
    // when no multi-tx clone was in progress (e.g., CloneAccountInit failed early).
    if !is_pending_clone(&pubkey) {
        ic_msg!(
            invoke_context,
            "CleanupPartialClone: account {} is not in pending clone state; refusing to delete",
            pubkey
        );
        return Err(MagicBlockProgramError::NoPendingClone.into());
    }

    let ctx = transaction_context.get_current_instruction_context()?;
    let auth_acc = transaction_context.get_account_at_index(
        ctx.get_index_of_instruction_account_in_transaction(0)?,
    )?;

    let tx_idx = validate_and_get_index(
        transaction_context,
        1,
        &pubkey,
        "CleanupPartialClone",
        invoke_context,
    )?;
    let account = transaction_context.get_account_at_index(tx_idx)?;

    ic_msg!(
        invoke_context,
        "CleanupPartialClone: cleaning up '{}'",
        pubkey
    );

    let current_lamports = account.borrow().lamports();
    let lamports_delta = -(current_lamports as i64);

    close_buffer_account(account);

    adjust_authority_lamports(auth_acc, lamports_delta)?;
    remove_pending_clone(&pubkey);
    Ok(())
}
