//! Single-transaction account cloning for accounts <63KB.

use std::collections::HashSet;

use magicblock_magic_program_api::instruction::AccountCloneFields;
use solana_account::ReadableAccount;
use solana_instruction::{error::InstructionError, Instruction};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

use super::{
    adjust_authority_lamports, set_account_from_fields, validate_and_get_index,
    validate_authority, validate_mutable,
    validate_post_delegation_action_sibling, validate_remote_slot,
};

/// Clones an account atomically in a single transaction.
///
/// Used for accounts that fit within transaction size limits (<63KB data).
/// Sets all account fields atomically: lamports, owner, data, executable, delegated, etc.
pub(crate) fn process_clone_account(
    signers: &HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    pubkey: Pubkey,
    data: Vec<u8>,
    fields: AccountCloneFields,
    actions: Vec<Instruction>,
) -> Result<(), InstructionError> {
    validate_authority(signers, invoke_context)?;
    if !actions.is_empty() {
        validate_post_delegation_action_sibling(
            invoke_context,
            &pubkey,
            fields.delegated,
            &actions,
        )?;
    }

    ic_msg!(
        invoke_context,
        "CloneAccount: cloning '{}', data_len={}",
        pubkey,
        data.len()
    );

    {
        let transaction_context = &invoke_context.transaction_context;
        let ctx = transaction_context.get_current_instruction_context()?;
        let mut auth_acc = transaction_context.accounts().try_borrow_mut(
            ctx.get_index_of_instruction_account_in_transaction(0)?,
        )?;

        let tx_idx = validate_and_get_index(
            transaction_context,
            1,
            &pubkey,
            "CloneAccount",
            invoke_context,
        )?;
        let mut account =
            transaction_context.accounts().try_borrow_mut(tx_idx)?;

        validate_mutable(&account, &pubkey, invoke_context)?;
        validate_remote_slot(
            &mut account,
            &pubkey,
            Some(fields.remote_slot),
            invoke_context,
        )?;

        let current_lamports = account.lamports();
        let lamports_delta = fields.lamports as i64 - current_lamports as i64;

        set_account_from_fields(
            invoke_context,
            &pubkey,
            account,
            &data,
            &fields,
        )?;
        adjust_authority_lamports(&mut auth_acc, lamports_delta)?;
    }

    Ok(())
}
