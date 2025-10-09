use magicblock_magic_program_api::TASK_CONTEXT_PUBKEY;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk::instruction::InstructionError;

use crate::utils::accounts::get_instruction_pubkey_with_idx;

pub(crate) fn check_task_context_id(
    invoke_context: &InvokeContext,
    idx: u16,
) -> Result<(), InstructionError> {
    let provided_magic_context = get_instruction_pubkey_with_idx(
        invoke_context.transaction_context,
        idx,
    )?;
    if !provided_magic_context.eq(&TASK_CONTEXT_PUBKEY) {
        ic_msg!(
            invoke_context,
            "ERR: invalid task context account {}",
            provided_magic_context
        );
        return Err(InstructionError::MissingAccount);
    }

    Ok(())
}
