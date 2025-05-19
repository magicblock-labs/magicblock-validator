mod process_accept_scheduled_commits;
mod process_schedule_action;
mod process_schedule_commit;
#[cfg(test)]
mod process_schedule_commit_tests;
mod process_scheduled_commit_sent;
pub(crate) mod transaction_scheduler;

use std::sync::atomic::AtomicU64;

use magicblock_core::magic_program::MAGIC_CONTEXT_PUBKEY;
pub(crate) use process_accept_scheduled_commits::*;
pub(crate) use process_schedule_action::*;
pub(crate) use process_schedule_commit::*;
pub use process_scheduled_commit_sent::{
    process_scheduled_commit_sent, register_scheduled_commit_sent, SentCommit,
};
use solana_log_collector::ic_msg;
use solana_program_runtime::{
    __private::InstructionError, invoke_context::InvokeContext,
};

use crate::utils::accounts::get_instruction_pubkey_with_idx;

pub(crate) static COMMIT_ID: AtomicU64 = AtomicU64::new(0);

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
