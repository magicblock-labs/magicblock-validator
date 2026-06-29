mod process_cancel_task;
mod process_schedule_task;

use magicblock_magic_program_api::instruction::MagicBlockInstruction;
pub(crate) use process_cancel_task::*;
pub(crate) use process_schedule_task::*;
use solana_instruction::{error::InstructionError, Instruction};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;

use crate::validator::effective_validator_authority_id;

// Assert that the task instructions do not have signers
// Assert they don't use the validator either
// Assert they are not a privileged instruction
pub(crate) fn validate_cranks_instructions(
    invoke_context: &mut InvokeContext,
    instructions: &[Instruction],
) -> Result<(), InstructionError> {
    for instruction in instructions {
        for account in &instruction.accounts {
            if account.is_signer {
                ic_msg!(
                    invoke_context,
                    "Crank ERR: only the crank signer PDA can be a signer in cranks (invalid signer: '{}')",
                    account.pubkey,
                );
                return Err(InstructionError::MissingRequiredSignature);
            } else if account.pubkey.eq(&effective_validator_authority_id()) {
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
            bincode::deserialize::<MagicBlockInstruction>(&instruction.data)
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
