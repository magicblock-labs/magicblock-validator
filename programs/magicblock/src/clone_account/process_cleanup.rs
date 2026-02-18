//! Cleanup for failed multi-transaction clones.

use std::collections::HashSet;

use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionContext;

use super::{
    adjust_authority_lamports, remove_pending_clone, validate_and_get_index,
    validate_authority,
};

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
    remove_pending_clone(&pubkey);

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

    {
        let mut acc = account.borrow_mut();
        acc.set_lamports(0);
        acc.set_data_from_slice(&[]);
        acc.set_executable(false);
        acc.set_owner(Pubkey::default());
        acc.set_delegated(false);
        acc.set_confined(false);
        acc.set_remote_slot(0);
        acc.set_undelegating(false);
    }

    adjust_authority_lamports(auth_acc, lamports_delta)?;
    Ok(())
}
