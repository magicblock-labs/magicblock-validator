//! Single-transaction account cloning for accounts <63KB.

use std::collections::HashSet;

use magicblock_magic_program_api::instruction::AccountCloneFields;
use solana_account::ReadableAccount;
use solana_instruction::{error::InstructionError, Instruction};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use solana_transaction_context::transaction_accounts::TransactionAccountViewMut;

use super::{
    adjust_authority_lamports, set_account_from_fields, validate_and_get_index,
    validate_authority, validate_mutable,
    validate_post_delegation_action_sibling, validate_remote_slot,
};

const RECOVERY_EPHEMERAL_CLONE_MARKER: &[u8] = b"MB_RECOVER_EPHEMERAL_V1";
const MAX_RECOVERY_EPHEMERAL_DATA_LEN: usize = 2048;
const RECOVERY_EPHEMERAL_OWNER_PROGRAMS: [Pubkey; 2] = [
    Pubkey::from_str_const("CyurcRVPuNLbCTBFeqRd3hz2iAA6uqYaBfnDDGeEWCHx"),
    Pubkey::from_str_const("DZtWbzgheM9YEaQu24dR3bkvWHURhSZw5jFwZyoz95DH"),
];

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
    let recovery_ephemeral_clone = is_recovery_ephemeral_clone_marker(&actions);
    if !recovery_ephemeral_clone && !actions.is_empty() {
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

        if recovery_ephemeral_clone {
            validate_recovery_ephemeral_clone(
                invoke_context,
                &pubkey,
                &account,
                &data,
                &fields,
            )?;
        } else {
            validate_mutable(&account, &pubkey, invoke_context)?;
        }
        validate_remote_slot(
            &mut account,
            &pubkey,
            Some(fields.remote_slot),
            invoke_context,
        )?;

        let current_lamports = account.lamports();
        let lamports_delta = fields.lamports as i64 - current_lamports as i64;

        if recovery_ephemeral_clone {
            ic_msg!(
                invoke_context,
                "CloneAccount: recovering ephemeral account '{}'",
                pubkey
            );
            account.set_ephemeral(true);
        }
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

fn is_recovery_ephemeral_clone_marker(actions: &[Instruction]) -> bool {
    matches!(actions, [ix]
        if ix.program_id == magicblock_magic_program_api::id()
            && ix.accounts.is_empty()
            && ix.data == RECOVERY_EPHEMERAL_CLONE_MARKER)
}

fn validate_recovery_ephemeral_clone(
    invoke_context: &InvokeContext,
    pubkey: &Pubkey,
    account: &TransactionAccountViewMut<'_>,
    data: &[u8],
    fields: &AccountCloneFields,
) -> Result<(), InstructionError> {
    if data.len() > MAX_RECOVERY_EPHEMERAL_DATA_LEN {
        ic_msg!(
            invoke_context,
            "CloneAccount: ephemeral recovery data too large for {}: {} > {}",
            pubkey,
            data.len(),
            MAX_RECOVERY_EPHEMERAL_DATA_LEN
        );
        return Err(InstructionError::InvalidInstructionData);
    }
    if fields.lamports != 0
        || fields.executable
        || fields.delegated
        || fields.confined
    {
        ic_msg!(
            invoke_context,
            "CloneAccount: invalid ephemeral recovery fields for {}",
            pubkey
        );
        return Err(InstructionError::InvalidArgument);
    }
    if !RECOVERY_EPHEMERAL_OWNER_PROGRAMS.contains(&fields.owner) {
        ic_msg!(
            invoke_context,
            "CloneAccount: invalid ephemeral recovery owner for {}: {}",
            pubkey,
            fields.owner
        );
        return Err(InstructionError::InvalidAccountOwner);
    }
    if account.ephemeral()
        || account.delegated()
        || account.undelegating()
        || account.confined()
        || account.executable()
        || account.lamports() != 0
        || !account.data().is_empty()
    {
        ic_msg!(
            invoke_context,
            "CloneAccount: ephemeral recovery target {} is not empty",
            pubkey
        );
        return Err(InstructionError::AccountAlreadyInitialized);
    }
    if account.owner() != &Pubkey::default()
        && account.owner() != &system_program::id()
    {
        ic_msg!(
            invoke_context,
            "CloneAccount: ephemeral recovery target {} has owner {}",
            pubkey,
            account.owner()
        );
        return Err(InstructionError::InvalidAccountOwner);
    }

    Ok(())
}
