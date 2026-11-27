// The crank signer is deprecated for programs but still executed here.
#![allow(deprecated)]

mod process_cancel_task;
mod process_execute_crank;
mod process_schedule_task;

use magic_program_api::{CRANK_PROGRAM_ID, instruction::MagicBlockInstruction};
pub(crate) use process_cancel_task::*;
pub(crate) use process_execute_crank::*;
pub(crate) use process_schedule_task::*;
use solana_instruction::{Instruction, error::InstructionError};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

use crate::validator::authority as engine_authority;

// Assert that the task instructions do not have signers aside from
// `crank_signer`, the task authority's crank signer, which must stay read-only
// Assert they don't use the validator either
// Assert they don't invoke the Crank program themselves
// Assert they are not a privileged instruction
pub(crate) fn validate_cranks_instructions(
    invoke_context: &mut InvokeContext<'_, '_>,
    crank_signer: &Pubkey,
    instructions: &[Instruction],
) -> Result<(), InstructionError> {
    for instruction in instructions {
        if instruction.program_id == CRANK_PROGRAM_ID {
            ic_msg!(
                invoke_context,
                "Crank ERR: scheduled crank instructions cannot invoke the Crank program",
            );
            return Err(InstructionError::IncorrectProgramId);
        }
        for account in &instruction.accounts {
            if account.is_signer && account.pubkey != *crank_signer {
                ic_msg!(
                    invoke_context,
                    "Crank ERR: only the crank signer PDA can be a signer in cranks (invalid signer: '{}')",
                    account.pubkey,
                );
                return Err(InstructionError::MissingRequiredSignature);
            } else if account.is_writable && account.pubkey == *crank_signer {
                ic_msg!(
                    invoke_context,
                    "Crank ERR: the crank signer PDA cannot be a writable account in cranks",
                );
                return Err(InstructionError::Immutable);
            } else if account.pubkey.eq(&engine_authority()) {
                ic_msg!(
                    invoke_context,
                    "Crank ERR: the validator authority cannot be used in cranks",
                );
                return Err(InstructionError::IncorrectAuthority);
            }
        }

        if !instruction.program_id.eq(&crate::ID) {
            continue;
        }

        let Ok(decoded_instruction) =
            wincode::deserialize::<MagicBlockInstruction>(&instruction.data)
        else {
            // Not a MagicBlock instruction, skip
            continue;
        };

        use MagicBlockInstruction::*;
        match decoded_instruction {
            ModifyAccounts { .. }
            | CloneAccount { .. }
            | CloneAccountInit { .. }
            | CloneAccountContinue { .. }
            | SetProgramAuthority { .. }
            | DisableExecutableCheck
            | EnableExecutableCheck
            | FinalizeProgramFromBuffer { .. }
            | FinalizeV1ProgramFromBuffer { .. }
            | CleanupPartialClone { .. } => {
                ic_msg!(
                    invoke_context,
                    "Crank ERR: privileged instruction is not allowed in cranks",
                );
                return Err(InstructionError::InvalidInstructionData);
            }
            _ => continue,
        }
    }
    Ok(())
}
