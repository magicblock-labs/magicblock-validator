//! Shared utilities for clone account and mutate account instruction processing.

use std::collections::HashSet;

use magicblock_magic_program_api::{
    instruction::{
        AccountCloneFields, PostDelegationActionExecutorInstruction,
    },
    POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID,
};
use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::{error::InstructionError, Instruction};
use solana_loader_v4_interface::state::LoaderV4State;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_transaction_context::{
    transaction_accounts::{AccountRefMut, TransactionAccountViewMut},
    TransactionContext,
};

use crate::{
    errors::MagicBlockProgramError,
    utils::instruction_sysvar,
    validator::{effective_validator_authority_id, validator_authority_id},
};

/// Converts a LoaderV4State reference to a byte slice.
///
/// # Safety
///
/// LoaderV4State is a POD type with no uninitialized padding bytes,
/// making it safe to reinterpret as a raw byte slice.
pub fn loader_v4_state_to_bytes(state: &LoaderV4State) -> &[u8] {
    let header_size = LoaderV4State::program_data_offset();
    // SAFETY: LoaderV4State is POD with no uninitialized padding
    unsafe {
        std::slice::from_raw_parts(
            (state as *const LoaderV4State) as *const u8,
            header_size,
        )
    }
}

/// Validates that the validator authority has signed the transaction.
pub fn validate_authority(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
) -> Result<(), InstructionError> {
    let auth = effective_validator_authority_id();
    if signers.contains(&auth) {
        return Ok(());
    }
    ic_msg!(
        invoke_context,
        "Validator authority {} not in signers",
        auth
    );
    Err(InstructionError::MissingRequiredSignature)
}

/// Validates that the account at `ix_index` matches `expected` pubkey.
/// Returns the transaction-level index on success.
pub fn validate_and_get_index(
    transaction_context: &TransactionContext,
    ix_index: u16,
    expected: &Pubkey,
    name: &str,
    invoke_context: &InvokeContext,
) -> Result<u16, InstructionError> {
    let ctx = transaction_context.get_current_instruction_context()?;
    let tx_idx =
        ctx.get_index_of_instruction_account_in_transaction(ix_index)?;
    let key = transaction_context.get_key_of_account_at_index(tx_idx)?;
    if *key == *expected {
        return Ok(tx_idx);
    }
    ic_msg!(
        invoke_context,
        "{}: key mismatch, expected {}, got {}",
        name,
        expected,
        key
    );
    Err(InstructionError::InvalidArgument)
}

/// Validates that a delegated account is undelegating (mutation allowed).
pub fn validate_not_delegated(
    acc: &TransactionAccountViewMut<'_>,
    pubkey: &Pubkey,
    invoke_context: &InvokeContext,
) -> Result<(), InstructionError> {
    let (is_delegated, is_undelegating) =
        { (acc.delegated(), acc.undelegating()) };
    if is_delegated && !is_undelegating {
        ic_msg!(
            invoke_context,
            "Account {} is delegated and not undelegating",
            pubkey
        );
        return Err(MagicBlockProgramError::AccountIsDelegated.into());
    }
    Ok(())
}

/// Validates that the account can be mutated (not ephemeral, not active delegated).
pub fn validate_mutable(
    account: &TransactionAccountViewMut,
    pubkey: &Pubkey,
    invoke_context: &InvokeContext,
) -> Result<(), InstructionError> {
    if account.ephemeral() {
        ic_msg!(
            invoke_context,
            "Account {} is ephemeral and cannot be mutated",
            pubkey
        );
        return Err(MagicBlockProgramError::AccountIsEphemeral.into());
    }
    validate_not_delegated(account, pubkey, invoke_context)
}

/// Validates that incoming remote_slot is not older than current.
/// Skips check if incoming_remote_slot is None.
pub fn validate_remote_slot(
    account: &mut TransactionAccountViewMut<'_>,
    pubkey: &Pubkey,
    incoming_remote_slot: Option<u64>,
    invoke_context: &InvokeContext,
) -> Result<(), InstructionError> {
    let Some(incoming) = incoming_remote_slot else {
        return Ok(());
    };
    let current = account.remote_slot();

    if incoming < current {
        ic_msg!(
            invoke_context,
            "Account {} incoming remote_slot {} is older than current {}; rejected",
            pubkey, incoming, current
        );
        return Err(MagicBlockProgramError::OutOfOrderUpdate.into());
    }

    ic_msg!(
        invoke_context,
        "incoming_slot = {}, current_slot = {}",
        incoming,
        current
    );

    Ok(())
}

/// Adjusts validator authority lamports by delta.
/// Positive delta = debit, negative delta = credit.
pub fn adjust_authority_lamports(
    auth_acc: &mut TransactionAccountViewMut,
    delta: i64,
) -> Result<(), InstructionError> {
    if delta == 0 {
        return Ok(());
    }
    let auth_lamports = auth_acc.lamports();
    let adjusted = if delta > 0 {
        auth_lamports
            .checked_sub(delta as u64)
            .ok_or(InstructionError::InsufficientFunds)?
    } else {
        auth_lamports
            .checked_add(delta.unsigned_abs())
            .ok_or(InstructionError::ArithmeticOverflow)?
    };
    auth_acc.set_lamports(adjusted);
    Ok(())
}

const INSTRUCTIONS_SYSVAR_IDX: u16 = 2;

pub fn validate_post_delegation_action_sibling(
    invoke_context: &mut InvokeContext,
    pubkey: &Pubkey,
    delegated_target: bool,
    actions: &[Instruction],
) -> Result<(), InstructionError> {
    validate_post_delegation_actions_payload(
        invoke_context,
        pubkey,
        delegated_target,
        actions,
    )?;

    let mut sysvar_data = load_instructions_sysvar_data(invoke_context)?;
    let current_index =
        instruction_sysvar::load_current_index(&mut sysvar_data)?;
    let next_instruction = instruction_sysvar::load_instruction_at(
        &mut sysvar_data,
        current_index.saturating_add(1),
    )
    .map_err(|_| -> InstructionError {
        ic_msg!(
            invoke_context,
            "Post-delegation action executor missing after clone for {}",
            pubkey
        );
        MagicBlockProgramError::PostDelegationActionExecutorMissing.into()
    })?;

    if next_instruction.program_id != POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID
    {
        ic_msg!(
            invoke_context,
            "Post-delegation action executor mismatch after clone for {}",
            pubkey
        );
        return Err(
            MagicBlockProgramError::PostDelegationActionExecutorMismatch.into(),
        );
    }

    let instruction: PostDelegationActionExecutorInstruction =
        bincode::deserialize(&next_instruction.data)
            .map_err(|_| InstructionError::InvalidInstructionData)?;
    match instruction {
        PostDelegationActionExecutorInstruction::Execute {
            cloned_account_pubkey: next_pubkey,
            actions: next_actions,
        } if next_pubkey == *pubkey && next_actions == actions => Ok(()),
        _ => {
            ic_msg!(
                invoke_context,
                "Post-delegation action executor payload mismatch for {}",
                pubkey
            );
            Err(MagicBlockProgramError::PostDelegationActionExecutorMismatch
                .into())
        }
    }
}

pub fn validate_post_delegation_actions_payload(
    invoke_context: &mut InvokeContext,
    pubkey: &Pubkey,
    delegated_target: bool,
    actions: &[Instruction],
) -> Result<(), InstructionError> {
    if actions.is_empty() {
        return Ok(());
    }
    if !delegated_target {
        ic_msg!(
            invoke_context,
            "Post-delegation actions require delegated clone target {}",
            pubkey
        );
        return Err(
            MagicBlockProgramError::PostDelegationActionsRequireDelegatedClone
                .into(),
        );
    }

    let validator_authority = validator_authority_id();
    let effective_validator_authority = effective_validator_authority_id();
    for action in actions {
        let signers = post_delegation_action_signers(action);
        if signers.contains(&validator_authority)
            || signers.contains(&effective_validator_authority)
        {
            ic_msg!(
                invoke_context,
                "Post-delegation action for {} uses validator authority as signer",
                pubkey
            );
            return Err(
                MagicBlockProgramError::PostDelegationActionUsesValidatorAuthority
                    .into(),
            );
        }
    }

    Ok(())
}

pub fn execute_post_delegation_actions(
    invoke_context: &mut InvokeContext,
    pubkey: &Pubkey,
    delegated_target: bool,
    actions: Vec<Instruction>,
) -> Result<(), InstructionError> {
    validate_post_delegation_actions_payload(
        invoke_context,
        pubkey,
        delegated_target,
        &actions,
    )?;
    for action in actions {
        let signers = post_delegation_action_signers(&action);
        invoke_context.native_invoke(action, &signers)?;
    }
    Ok(())
}

pub fn load_instructions_sysvar_data(
    invoke_context: &mut InvokeContext,
) -> Result<Vec<u8>, InstructionError> {
    let transaction_context = &invoke_context.transaction_context;
    let ctx = transaction_context.get_current_instruction_context()?;
    let tx_idx = ctx.get_index_of_instruction_account_in_transaction(
        INSTRUCTIONS_SYSVAR_IDX,
    )?;
    let key = transaction_context.get_key_of_account_at_index(tx_idx)?;
    if key != &solana_sdk_ids::sysvar::instructions::id() {
        ic_msg!(
            invoke_context,
            "Instructions sysvar account missing at index {}",
            INSTRUCTIONS_SYSVAR_IDX
        );
        return Err(InstructionError::UnsupportedSysvar);
    }
    Ok(transaction_context
        .accounts()
        .try_borrow(tx_idx)?
        .data()
        .to_vec())
}

fn post_delegation_action_signers(action: &Instruction) -> Vec<Pubkey> {
    action
        .accounts
        .iter()
        .filter_map(|account| account.is_signer.then_some(account.pubkey))
        .collect()
}

/// Closes a buffer/temporary account by resetting it to default state.
/// The account will be removed from accountsdb due to the ephemeral flag.
pub fn close_buffer_account(mut acc: AccountRefMut<'_>) {
    acc.set_lamports(0);
    acc.set_owner(Pubkey::default());
    acc.resize(0, 0);
    // Setting ephemeral flag on empty account, forces
    // accountsdb to remove it, thus reclaiming space
    acc.set_ephemeral(true);
    acc.set_delegated(false);
}

/// Returns the deploy slot for program cloning (current_slot - 5).
/// This bypasses LoaderV4's cooldown mechanism by simulating the program
/// was deployed 5 slots ago.
pub fn get_deploy_slot(invoke_context: &InvokeContext) -> u64 {
    invoke_context
        .get_sysvar_cache()
        .get_clock()
        .map(|clock| clock.slot.saturating_sub(5))
        .unwrap_or(0)
}

/// Calculates rent-exempt lamports for the given data length using the Rent sysvar.
pub fn minimum_balance(
    invoke_context: &InvokeContext,
    data_len: usize,
) -> Result<u64, InstructionError> {
    invoke_context
        .get_sysvar_cache()
        .get_rent()
        .map(|rent| rent.minimum_balance(data_len))
}

/// Sets account fields from AccountCloneFields and data.
pub fn set_account_from_fields(
    invoke_context: &InvokeContext,
    pubkey: &Pubkey,
    mut acc: AccountRefMut<'_>,
    data: &[u8],
    fields: &AccountCloneFields,
) -> Result<(), InstructionError> {
    ic_msg!(
        invoke_context,
        "src account state: lamports={}, owner={}, executable={}, delegated={}, confined={}, remote_slot={}, data_len={}",
        fields.lamports,
        fields.owner,
        fields.executable,
        fields.delegated,
        fields.confined,
        fields.remote_slot,
        data.len()
    );
    ic_msg!(
        invoke_context,
        "dest account state: lamports={}, owner={}, executable={}, delegated={}, undelegating={} confined={}, remote_slot={}, data_len={}",
        acc.lamports(),
        acc.owner(),
        acc.executable(),
        acc.delegated(),
        acc.undelegating(),
        acc.confined(),
        acc.remote_slot(),
        acc.data().len()
    );

    // A delegated clone is still allowed to override a plain clone, which can
    // happen when an eATA projection catches up after the underlying ATA was
    // cloned without delegation metadata. Same-slot delegated refreshes over an
    // already delegated/undelegating target would re-run post-delegation actions
    // for the same delegation event and are rejected.
    if fields.delegated
        && fields.owner == dlp_api::id()
        && !is_validator_magic_fee_vault(pubkey)
    {
        return Err(
            MagicBlockProgramError::DelegatedCloneOwnerIsDelegationProgram
                .into(),
        );
    } else if fields.delegated
        && (acc.delegated() || acc.undelegating())
        && acc.remote_slot() == fields.remote_slot
    {
        return Err(
            MagicBlockProgramError::DuplicateDelegatedAccountClone.into()
        );
    } else if acc.remote_slot() > fields.remote_slot {
        return Err(MagicBlockProgramError::OutOfOrderUpdate.into());
    }

    acc.set_lamports(fields.lamports);
    acc.set_owner(fields.owner);
    acc.set_data_from_slice(data);
    acc.set_executable(fields.executable);
    acc.set_delegated(fields.delegated);
    acc.set_confined(fields.confined);
    acc.set_remote_slot(fields.remote_slot);
    acc.set_undelegating(false);

    Ok(())
}

fn is_validator_magic_fee_vault(pubkey: &Pubkey) -> bool {
    pubkey
        == &dlp_api::pda::magic_fee_vault_pda_from_validator(
            &validator_authority_id(),
        )
}
