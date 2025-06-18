use std::{collections::HashSet, sync::atomic::Ordering};

use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk::{
    instruction::InstructionError, pubkey::Pubkey,
    transaction_context::TransactionContext,
};

use crate::{
    args::MagicL1MessageArgs,
    magic_schedule_l1_message::{ConstructionContext, ScheduledL1Message},
    schedule_transactions::{
        check_magic_context_id,
        schedule_l1_message_processor::process_scheddule_l1_message,
        MESSAGE_ID,
    },
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
    TransactionScheduler,
};

const PAYER_IDX: u16 = 0;
const MAGIC_CONTEXT_IDX: u16 = PAYER_IDX + 1;
const ACTION_ACCOUNTS_OFFSET: usize = MAGIC_CONTEXT_IDX as usize + 1;
const ACTIONS_SUPPORTED: bool = false;

pub(crate) fn process_schedule_l1_message(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    args: MagicL1MessageArgs,
) -> Result<(), InstructionError> {
    // TODO: remove once actions are supported
    if !ACTIONS_SUPPORTED {
        return Err(InstructionError::InvalidInstructionData);
    }

    check_magic_context_id(invoke_context, MAGIC_CONTEXT_IDX)?;

    let transaction_context = &invoke_context.transaction_context.clone();
    let ix_ctx = transaction_context.get_current_instruction_context()?;

    // Assert MagicBlock program
    ix_ctx
        .find_index_of_program_account(transaction_context, &crate::id())
        .ok_or_else(|| {
            ic_msg!(
                invoke_context,
                "ScheduleAction ERR: Magic program account not found"
            );
            InstructionError::UnsupportedProgramId
        })?;

    // Assert enough accounts
    let ix_accs_len = ix_ctx.get_number_of_instruction_accounts() as usize;
    if ix_accs_len <= ACTION_ACCOUNTS_OFFSET {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: not enough accounts to schedule commit ({}), need payer, signing program an account for each pubkey to be committed",
            ix_accs_len
        );
        return Err(InstructionError::NotEnoughAccountKeys);
    }

    // Assert Payer is signer
    let payer_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, PAYER_IDX)?;
    if !signers.contains(payer_pubkey) {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: payer pubkey {} not in signers",
            payer_pubkey
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    //
    // Get the program_id of the parent instruction that invoked this one via CPI
    //

    // We cannot easily simulate the transaction being invoked via CPI
    // from the owning program during unit tests
    // Instead the integration tests ensure that this works as expected
    let parent_program_id =
        get_parent_program_id(transaction_context, invoke_context)?;

    // It appears that in builtin programs `Clock::get` doesn't work as expected, thus
    // we have to get it directly from the sysvar cache.
    let clock =
        invoke_context
            .get_sysvar_cache()
            .get_clock()
            .map_err(|err| {
                ic_msg!(invoke_context, "Failed to get clock sysvar: {}", err);
                InstructionError::UnsupportedSysvar
            })?;

    // Determine id and slot
    let message_id = MESSAGE_ID.fetch_add(1, Ordering::Relaxed);
    let construction_context = ConstructionContext::new(
        parent_program_id,
        &signers,
        transaction_context,
        invoke_context,
    );
    let scheduled_action = ScheduledL1Message::try_new(
        &args,
        message_id,
        clock.slot,
        payer_pubkey,
        &construction_context,
    )?;
    // TODO: move all logic to some Processor
    // Rn this just locks accounts
    process_scheddule_l1_message(&construction_context, &args)?;

    let action_sent_signature =
        scheduled_action.action_sent_transaction.signatures[0];

    let context_acc = get_instruction_account_with_idx(
        transaction_context,
        MAGIC_CONTEXT_IDX,
    )?;
    TransactionScheduler::schedule_l1_message(
        invoke_context,
        context_acc,
        scheduled_action,
    )
    .map_err(|err| {
        ic_msg!(
            invoke_context,
            "ScheduleAction ERR: failed to schedule action: {}",
            err
        );
        InstructionError::GenericError
    })?;
    ic_msg!(invoke_context, "Scheduled commit with ID: {}", message_id);
    ic_msg!(
        invoke_context,
        "ScheduledCommitSent signature: {}",
        action_sent_signature,
    );

    Ok(())
}

#[cfg(not(test))]
fn get_parent_program_id(
    transaction_context: &TransactionContext,
    invoke_context: &mut InvokeContext,
) -> Result<Option<Pubkey>, InstructionError> {
    let frames = crate::utils::instruction_context_frames::InstructionContextFrames::try_from(transaction_context)?;
    let parent_program_id =
        frames.find_program_id_of_parent_of_current_instruction();

    ic_msg!(
        invoke_context,
        "ScheduleCommit: parent program id: {}",
        parent_program_id
            .map_or_else(|| "None".to_string(), |id| id.to_string())
    );

    Ok(parent_program_id.map(Clone::clone))
}

#[cfg(test)]
fn get_parent_program_id(
    transaction_context: &TransactionContext,
    _: &mut InvokeContext,
) -> Result<Option<Pubkey>, InstructionError> {
    use solana_sdk::account::ReadableAccount;

    use crate::utils::accounts::get_instruction_account_with_idx;

    let first_committee_owner = *get_instruction_account_with_idx(
        transaction_context,
        ACTION_ACCOUNTS_OFFSET as u16,
    )?
    .borrow()
    .owner();

    Ok(Some(first_committee_owner.clone()))
}
