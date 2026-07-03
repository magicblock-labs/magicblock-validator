use std::collections::HashSet;

use magicblock_core::intent::{
    types::CommittedAccount, CommitAndUndelegate, CommitType, MagicBaseIntent,
    UndelegateType,
};
use magicblock_magic_program_api::{
    instruction::MagicBlockInstruction, MAGIC_CONTEXT_PUBKEY,
};
use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

use crate::{
    clone_account::remove_pending_clone,
    magic_scheduled_base_intent::ScheduledIntentBundle,
    schedule_transactions::get_clock,
    utils::{
        account_actions::mark_account_as_undelegated,
        accounts::{
            get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
        },
        instruction_utils::InstructionUtils,
        validation::validate_last_after_clone,
    },
    validator::validator_authority_id,
    MagicContext,
};

const PAYER_IDX: u16 = 0;
const CLONED_ACCOUNT_IDX: u16 = 1;
const INSTRUCTIONS_SYSVAR_IDX: u16 = 2;
const MAGIC_CONTEXT_ACCOUNT_IDX: u16 = 3;

pub(crate) fn process_schedule_cloned_account_undelegation(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    cloned_account_pubkey: Pubkey,
) -> Result<(), InstructionError> {
    validate_authority(&signers, invoke_context)?;
    let previous_was_final_continue =
        validate_previous_clone(invoke_context, cloned_account_pubkey)?;
    let clock = get_clock(invoke_context)?;
    let blockhash = invoke_context.environment_config.blockhash;

    let transaction_context = &*invoke_context.transaction_context;
    let cloned_account_pubkey_at_idx = get_instruction_pubkey_with_idx(
        transaction_context,
        CLONED_ACCOUNT_IDX,
    )?;
    if cloned_account_pubkey_at_idx != &cloned_account_pubkey {
        ic_msg!(
            invoke_context,
            "ScheduleClonedAccountUndelegation ERR: cloned account mismatch, expected {}, got {}",
            cloned_account_pubkey,
            cloned_account_pubkey_at_idx
        );
        return Err(InstructionError::InvalidArgument);
    }

    check_instructions_sysvar(invoke_context)?;
    check_magic_context(invoke_context)?;

    let cloned_account = get_instruction_account_with_idx(
        transaction_context,
        CLONED_ACCOUNT_IDX,
    )?;
    let account = cloned_account.to_account_shared_data()?;
    if !account.delegated() || account.confined() || account.ephemeral() {
        ic_msg!(
            invoke_context,
            "ScheduleClonedAccountUndelegation ERR: account {} must be delegated, non-confined, and non-ephemeral",
            cloned_account_pubkey
        );
        return Err(InstructionError::InvalidAccountData);
    }

    mark_account_as_undelegated(&cloned_account)?;
    ic_msg!(
        invoke_context,
        "ScheduleClonedAccountUndelegation: Marking account {} as undelegating",
        cloned_account_pubkey
    );

    let committed = CommittedAccount::from_account_shared(
        cloned_account_pubkey,
        &account,
        None,
    );

    let context_acc = get_instruction_account_with_idx(
        transaction_context,
        MAGIC_CONTEXT_ACCOUNT_IDX,
    )?;
    let mut context = MagicContext::deserialize(context_acc.borrow()?.data())
        .map_err(|err| {
        ic_msg!(
            invoke_context,
            "Failed to deserialize MagicContext: {}",
            err
        );
        InstructionError::GenericError
    })?;

    let intent_id = context.next_intent_id();
    let sent_transaction =
        InstructionUtils::scheduled_commit_sent(intent_id, blockhash);

    let base_intent =
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action: CommitType::Standalone(vec![committed]),
            undelegate_action: UndelegateType::Standalone,
        })
        .into();

    let scheduled = ScheduledIntentBundle {
        id: intent_id,
        slot: clock.slot,
        blockhash,
        sent_transaction,
        payer: validator_authority_id(),
        intent_bundle: base_intent,
    };
    context.add_scheduled_action(scheduled);
    context.write_to(context_acc.borrow_mut()?.data_as_mut_slice())?;

    if previous_was_final_continue {
        remove_pending_clone(&cloned_account_pubkey);
    }

    ic_msg!(
        invoke_context,
        "ScheduleClonedAccountUndelegation: scheduled undelegation for {} with ID {}",
        cloned_account_pubkey,
        intent_id
    );
    Ok(())
}

fn validate_authority(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
) -> Result<(), InstructionError> {
    let payer_pubkey = get_instruction_pubkey_with_idx(
        invoke_context.transaction_context,
        PAYER_IDX,
    )?;
    if payer_pubkey != &validator_authority_id()
        || !signers.contains(payer_pubkey)
    {
        ic_msg!(
            invoke_context,
            "ScheduleClonedAccountUndelegation ERR: validator authority {} not in signers",
            payer_pubkey
        );
        return Err(InstructionError::MissingRequiredSignature);
    }
    Ok(())
}

/// Validates that the previous top-level instruction is the clone this
/// undelegation belongs to.
///
/// #Returns:
/// - `true` if the previous instruction was a final `CloneAccountContinue` for a chunked clone.
/// - `false` if the previous instruction was a `CloneAccount` for a plain clone.
fn validate_previous_clone(
    invoke_context: &mut InvokeContext,
    cloned_account_pubkey: Pubkey,
) -> Result<bool, InstructionError> {
    let previous_instruction = validate_last_after_clone(
        invoke_context,
        "ScheduleClonedAccountUndelegation",
    )?;

    let previous_magic_instruction: MagicBlockInstruction =
        bincode::deserialize(&previous_instruction.data)
            .map_err(|_| InstructionError::InvalidInstructionData)?;
    match previous_magic_instruction {
        MagicBlockInstruction::CloneAccount { pubkey, fields, .. }
            if pubkey == cloned_account_pubkey && fields.delegated =>
        {
            Ok(false)
        }
        MagicBlockInstruction::CloneAccountContinue {
            pubkey,
            is_last: true,
            ..
        } if pubkey == cloned_account_pubkey => Ok(true),
        _ => {
            ic_msg!(
                invoke_context,
                "ScheduleClonedAccountUndelegation previous instruction mismatch"
            );
            Err(InstructionError::InvalidInstructionData)
        }
    }
}

fn check_instructions_sysvar(
    invoke_context: &InvokeContext,
) -> Result<(), InstructionError> {
    let provided = get_instruction_pubkey_with_idx(
        invoke_context.transaction_context,
        INSTRUCTIONS_SYSVAR_IDX,
    )?;
    if provided != &solana_sdk_ids::sysvar::instructions::id() {
        ic_msg!(
            invoke_context,
            "ScheduleClonedAccountUndelegation ERR: instructions sysvar missing"
        );
        return Err(InstructionError::UnsupportedSysvar);
    }
    Ok(())
}

fn check_magic_context(
    invoke_context: &InvokeContext,
) -> Result<(), InstructionError> {
    let provided = get_instruction_pubkey_with_idx(
        invoke_context.transaction_context,
        MAGIC_CONTEXT_ACCOUNT_IDX,
    )?;
    if provided != &MAGIC_CONTEXT_PUBKEY {
        ic_msg!(
            invoke_context,
            "ScheduleClonedAccountUndelegation ERR: invalid magic context {}",
            provided
        );
        return Err(InstructionError::MissingAccount);
    }
    Ok(())
}
