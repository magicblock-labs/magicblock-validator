mod process_accept_scheduled_commits;
mod process_schedule_commit;
#[cfg(test)]
mod process_schedule_commit_tests;
mod process_schedule_intent_bundle;
mod process_scheduled_commit_sent;
pub(crate) mod transaction_scheduler;

use magicblock_core::intent::CommittedAccount;
use magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY;
pub(crate) use process_accept_scheduled_commits::*;
pub(crate) use process_schedule_commit::*;
pub(crate) use process_schedule_intent_bundle::process_schedule_intent_bundle;
pub use process_scheduled_commit_sent::{
    process_scheduled_commit_sent, register_scheduled_commit_sent, SentCommit,
};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;

use crate::{
    magic_sys::{
        fetch_current_commit_nonces, COMMIT_LIMIT, COMMIT_LIMIT_ERR,
        MISSING_COMMIT_NONCE_ERR,
    },
    utils::accounts::get_instruction_pubkey_with_idx,
};

pub(crate) fn check_commit_limits(
    commits: &[CommittedAccount],
    invoke_context: &InvokeContext,
) -> Result<(), InstructionError> {
    let mut nonces = fetch_current_commit_nonces(commits)?;
    let mut limit_exceeded = false;
    for account in commits {
        let nonce = nonces
            .remove(&account.pubkey)
            .ok_or(InstructionError::Custom(MISSING_COMMIT_NONCE_ERR))?;
        if nonce >= COMMIT_LIMIT {
            ic_msg!(
                invoke_context,
                "ScheduleCommit ERR: commit limit exceeded for account {}, only undelegation is allowed",
                account.pubkey
            );
            limit_exceeded = true;
        }
    }
    if limit_exceeded {
        Err(InstructionError::Custom(COMMIT_LIMIT_ERR))
    } else {
        Ok(())
    }
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
