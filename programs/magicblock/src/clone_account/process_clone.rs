//! Single-transaction account cloning for accounts <63KB.

use std::collections::HashSet;

use magicblock_magic_program_api::instruction::AccountCloneFields;
use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionContext;

use super::{adjust_authority_lamports, validate_and_get_index, validate_authority};

/// Clones an account atomically in a single transaction.
///
/// Used for accounts that fit within transaction size limits (<63KB data).
/// Sets all account fields atomically: lamports, owner, data, executable, delegated, etc.
pub(crate) fn process_clone_account(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    pubkey: Pubkey,
    data: Vec<u8>,
    fields: AccountCloneFields,
) -> Result<(), InstructionError> {
    validate_authority(signers, invoke_context)?;

    let ctx = transaction_context.get_current_instruction_context()?;
    let auth_acc = transaction_context.get_account_at_index(
        ctx.get_index_of_instruction_account_in_transaction(0)?,
    )?;

    let tx_idx = validate_and_get_index(transaction_context, 1, &pubkey, "CloneAccount", invoke_context)?;
    let account = transaction_context.get_account_at_index(tx_idx)?;

    ic_msg!(invoke_context, "CloneAccount: cloning '{}', data_len={}", pubkey, data.len());

    let current_lamports = account.borrow().lamports();
    let lamports_delta = fields.lamports as i64 - current_lamports as i64;

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
    Ok(())
}
