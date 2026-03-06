mod process_accept_scheduled_commits;
mod process_add_action_callback;
mod process_schedule_commit;
#[cfg(test)]
mod process_schedule_commit_tests;
mod process_schedule_intent_bundle;
mod process_scheduled_commit_sent;
pub(crate) mod transaction_scheduler;

use std::sync::Arc;

use magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY;
pub(crate) use process_accept_scheduled_commits::*;
pub(crate) use process_add_action_callback::process_add_action_callback;
pub(crate) use process_schedule_commit::*;
pub(crate) use process_schedule_intent_bundle::process_schedule_intent_bundle;
pub use process_scheduled_commit_sent::{
    process_scheduled_commit_sent, register_scheduled_commit_sent, SentCommit,
};
use solana_clock::Clock;
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionContext;

use crate::utils::accounts::get_instruction_pubkey_with_idx;

pub(crate) const PAYER_IDX: u16 = 0;
pub(crate) const MAGIC_CONTEXT_IDX: u16 = PAYER_IDX + 1;
pub(crate) const ACCOUNTS_OFFSET: usize = MAGIC_CONTEXT_IDX as usize + 1;

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

    Ok(parent_program_id.copied())
}

#[cfg(test)]
fn get_parent_program_id(
    transaction_context: &TransactionContext,
    _: &mut InvokeContext,
) -> Result<Option<Pubkey>, InstructionError> {
    use solana_account::ReadableAccount;

    use crate::utils::accounts::get_instruction_account_with_idx;

    let owner = *get_instruction_account_with_idx(
        transaction_context,
        ACCOUNTS_OFFSET as u16,
    )?
    .borrow()
    .owner();

    Ok(Some(owner))
}

pub(crate) fn get_clock(
    invoke_context: &mut InvokeContext,
) -> Result<Arc<Clock>, InstructionError> {
    invoke_context
        .get_sysvar_cache()
        .get_clock()
        .map_err(|err| {
            ic_msg!(invoke_context, "Failed to get clock sysvar: {}", err);
            InstructionError::UnsupportedSysvar
        })
}

pub fn check_magic_context_id(
    invoke_context: &InvokeContext,
    idx: u16,
) -> Result<(), InstructionError> {
    let provided_magic_context = get_instruction_pubkey_with_idx(
        invoke_context.transaction_context,
        idx,
    )?;
    if !provided_magic_context.eq(&MAGIC_CONTEXT_PUBKEY) {
        ic_msg!(
            invoke_context,
            "ERR: invalid magic context account {}",
            provided_magic_context
        );
        return Err(InstructionError::MissingAccount);
    }

    Ok(())
}
