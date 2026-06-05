//! Continuation of multi-transaction cloning for large accounts.

use std::collections::HashSet;

use solana_account::WritableAccount;
use solana_instruction::{error::InstructionError, Instruction};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

use super::{
    is_pending_clone, remove_pending_clone, validate_and_get_index,
    validate_authority, validate_post_delegation_action_sibling,
};
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
    invoke_context: &mut InvokeContext,
    pubkey: Pubkey,
    offset: u32,
    data: Vec<u8>,
    is_last: bool,
    actions: Vec<Instruction>,
) -> Result<(), InstructionError> {
    validate_authority(signers, invoke_context)?;

    if !is_last && !actions.is_empty() {
        ic_msg!(
            invoke_context,
            "CloneAccountContinue: actions require final clone chunk for {}",
            pubkey
        );
        return Err(InstructionError::InvalidArgument);
    }

    if !is_pending_clone(&pubkey) {
        ic_msg!(
            invoke_context,
            "CloneAccountContinue: no pending clone for {}",
            pubkey
        );
        return Err(MagicBlockProgramError::NoPendingClone.into());
    }

    let delegated_target = {
        let transaction_context = &invoke_context.transaction_context;
        let tx_idx = validate_and_get_index(
            transaction_context,
            1,
            &pubkey,
            "CloneAccountContinue",
            invoke_context,
        )?;
        ic_msg!(
            invoke_context,
            "CloneAccountContinue: '{}' offset={} len={} is_last={}",
            pubkey,
            offset,
            data.len(),
            is_last
        );

        let mut acc = transaction_context.accounts().try_borrow_mut(tx_idx)?;
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
        acc.delegated()
    };

    if is_last {
        if actions.is_empty() {
            remove_pending_clone(&pubkey);
        } else {
            validate_post_delegation_action_sibling(
                invoke_context,
                &pubkey,
                delegated_target,
                &actions,
            )?;
        }
        ic_msg!(
            invoke_context,
            "CloneAccountContinue: clone complete for '{}'",
            pubkey
        );
    }

    Ok(())
}
