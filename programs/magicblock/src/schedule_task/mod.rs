mod process_cancel_task;
mod process_execute_task;
mod process_schedule_task;

use magicblock_magic_program_api::pda::CRANK_SIGNER;
pub(crate) use process_cancel_task::*;
pub(crate) use process_execute_task::*;
pub(crate) use process_schedule_task::*;
use solana_instruction::{error::InstructionError, Instruction};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;

use crate::validator::validator_authority_id;

// Assert that the task instructions do not have signers aside from the crank signer
// Assert they don't use the validator either
pub(crate) fn validate_cranks_instructions(
    invoke_context: &mut InvokeContext,
    instructions: &[Instruction],
) -> Result<(), InstructionError> {
    for instruction in instructions {
        for account in &instruction.accounts {
            if account.is_signer && account.pubkey.ne(&CRANK_SIGNER) {
                ic_msg!(
                    invoke_context,
                    "ScheduleTask: only the crank signer PDA can be a signer in cranks (invalid signer: '{}')",
                    account.pubkey,
                );
                return Err(InstructionError::MissingRequiredSignature);
            } else if account.is_writable && account.pubkey.eq(&CRANK_SIGNER) {
                ic_msg!(
                    invoke_context,
                    "ScheduleTask: the crank signer PDA cannot be a writable account in cranks",
                );
                return Err(InstructionError::Immutable);
            } else if account.pubkey.eq(&validator_authority_id()) {
                ic_msg!(
                    invoke_context,
                    "ScheduleTask: the validator authority cannot be used in cranks",
                );
                return Err(InstructionError::IncorrectAuthority);
            }
        }
    }
    Ok(())
}
