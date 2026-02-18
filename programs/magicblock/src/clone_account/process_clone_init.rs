//! First step of multi-transaction cloning for large accounts (>=63KB).

use std::collections::HashSet;

use magicblock_magic_program_api::instruction::AccountCloneFields;
use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionContext;

use super::{add_pending_clone, adjust_authority_lamports, is_pending_clone, validate_and_get_index, validate_authority, validate_mutable, validate_remote_slot};
use crate::errors::MagicBlockProgramError;

/// Initializes a multi-transaction clone for a large account.
///
/// # Flow
///
/// 1. Creates account with zeroed buffer of `total_data_len` size
/// 2. Copies `initial_data` to offset 0
/// 3. Sets metadata fields (lamports, owner, etc.)
/// 4. Registers pubkey in `PENDING_CLONES`
///
/// Must be followed by `CloneAccountContinue` instructions until `is_last=true`.
pub(crate) fn process_clone_account_init(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    pubkey: Pubkey,
    total_data_len: u32,
    initial_data: Vec<u8>,
    fields: AccountCloneFields,
) -> Result<(), InstructionError> {
    validate_authority(signers, invoke_context)?;

    if is_pending_clone(&pubkey) {
        ic_msg!(invoke_context, "CloneAccountInit: account {} already has pending clone", pubkey);
        return Err(MagicBlockProgramError::CloneAlreadyPending.into());
    }

    if initial_data.len() > total_data_len as usize {
        ic_msg!(
            invoke_context,
            "CloneAccountInit: initial_data len {} exceeds total_data_len {}",
            initial_data.len(),
            total_data_len
        );
        return Err(InstructionError::InvalidArgument);
    }

    let ctx = transaction_context.get_current_instruction_context()?;
    let auth_acc = transaction_context.get_account_at_index(
        ctx.get_index_of_instruction_account_in_transaction(0)?,
    )?;

    let tx_idx = validate_and_get_index(transaction_context, 1, &pubkey, "CloneAccountInit", invoke_context)?;
    let account = transaction_context.get_account_at_index(tx_idx)?;

    // Prevent overwriting ephemeral or active delegated accounts
    validate_mutable(&account, &pubkey, invoke_context)?;
    // Prevent stale updates from overwriting fresher data
    validate_remote_slot(&account, &pubkey, Some(fields.remote_slot), invoke_context)?;

    ic_msg!(
        invoke_context,
        "CloneAccountInit: initializing '{}', total_len={}, initial_len={}",
        pubkey, total_data_len, initial_data.len()
    );

    let current_lamports = account.borrow().lamports();
    let lamports_delta = fields.lamports as i64 - current_lamports as i64;

    // Pre-allocate full buffer and copy initial chunk
    let mut data = vec![0u8; total_data_len as usize];
    data[..initial_data.len()].copy_from_slice(&initial_data);

    {
        let mut acc = account.borrow_mut();
        acc.set_lamports(fields.lamports);
        acc.set_owner(fields.owner);
        acc.set_data_from_slice(&data);
        acc.set_executable(fields.executable);
        acc.set_delegated(fields.delegated);
        acc.set_confined(fields.confined);
        acc.set_remote_slot(fields.remote_slot);
        acc.set_undelegating(false);
    }

    adjust_authority_lamports(auth_acc, lamports_delta)?;
    add_pending_clone(pubkey);
    Ok(())
}
