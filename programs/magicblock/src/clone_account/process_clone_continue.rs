//! Continuation of multi-transaction cloning for large accounts.

use std::collections::HashSet;

use solana_account::WritableAccount;
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionContext;

use super::{is_pending_clone, remove_pending_clone, validate_and_get_index, validate_authority};
use crate::errors::MagicBlockProgramError;

/// Writes a data chunk to a pending multi-transaction clone.
///
/// # Flow
///
/// 1. Verifies pubkey is in `PENDING_CLONES`
/// 2. Writes `data` at `offset` in the account's data buffer
/// 3. If `is_last=true`, removes from `PENDING_CLONES` (clone complete)
///
/// No lamports adjustment needed - account already has correct lamports from Init.
pub(crate) fn process_clone_account_continue(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    pubkey: Pubkey,
    offset: u32,
    data: Vec<u8>,
    is_last: bool,
) -> Result<(), InstructionError> {
    validate_authority(signers, invoke_context)?;

    if !is_pending_clone(&pubkey) {
        ic_msg!(invoke_context, "CloneAccountContinue: no pending clone for {}", pubkey);
        return Err(MagicBlockProgramError::NoPendingClone.into());
    }

    let tx_idx = validate_and_get_index(
        transaction_context,
        1,
        &pubkey,
        "CloneAccountContinue",
        invoke_context,
    )?;
    let account = transaction_context.get_account_at_index(tx_idx)?;

    ic_msg!(
        invoke_context,
        "CloneAccountContinue: '{}' offset={} len={} is_last={}",
        pubkey, offset, data.len(), is_last
    );

    // Write data at offset
    {
        let mut acc = account.borrow_mut();
        let account_data = acc.data_as_mut_slice();
        let start = offset as usize;
        let end = start + data.len();

        if end > account_data.len() {
            ic_msg!(
                invoke_context,
                "CloneAccountContinue: offset + len ({}) exceeds account len ({})",
                end, account_data.len()
            );
            return Err(InstructionError::InvalidArgument);
        }
        account_data[start..end].copy_from_slice(&data);
    }

    if is_last {
        remove_pending_clone(&pubkey);
        ic_msg!(invoke_context, "CloneAccountContinue: clone complete for '{}'", pubkey);
    }

    Ok(())
}
