use magicblock_magic_program_api::instruction::{
    CallbackInstruction, EphemeralSystemInstruction, MagicBlockInstruction,
    PostDelegationActionExecutorInstruction,
};
use solana_instruction::error::InstructionError;
use solana_program_runtime::{
    declare_process_instruction, invoke_context::InvokeContext,
};
use wincode::{SchemaRead, config::DefaultConfig};

use crate::{
    ephemeral_accounts::{
        process_close_ephemeral_account, process_create_ephemeral_account,
        process_resize_ephemeral_account,
    },
    errors::MagicBlockProgramError,
    process_scheduled_commit_sent,
    schedule_task::{
        process_cancel_task, process_execute_crank, process_schedule_task,
    },
    schedule_transactions::{
        ProcessScheduleCommitOptions, process_accept_scheduled_commits,
        process_add_action_callback, process_execute_callback,
        process_schedule_commit, process_schedule_intent_bundle,
    },
};

pub const DEFAULT_COMPUTE_UNITS: u64 = 150;

/// Rejects an instruction whose account-composition handler was removed.
///
/// The discriminants are retained so existing clients still deserialize and get
/// a defined error rather than a decode failure, but the engine composes
/// accounts now, so nothing here can service them.
fn composition_removed(
    invoke_context: &InvokeContext,
    instruction: &str,
) -> Result<(), InstructionError> {
    solana_log_collector::ic_msg!(
        invoke_context,
        "{} is no longer supported: account composition is handled by the engine",
        instruction
    );
    Err(MagicBlockProgramError::AccountCompositionRemoved.into())
}

fn deserialize_instruction<T>(
    invoke_context: &mut InvokeContext,
) -> Result<T, InstructionError>
where
    T: for<'de> SchemaRead<'de, DefaultConfig, Dst = T>,
{
    wincode::deserialize(
        invoke_context
            .transaction_context
            .get_current_instruction_context()?
            .get_instruction_data(),
    )
    .map_err(|_| InstructionError::InvalidInstructionData)
}

declare_process_instruction!(
    Entrypoint,
    DEFAULT_COMPUTE_UNITS,
    |invoke_context| {
        use MagicBlockInstruction::*;
        let instruction = deserialize_instruction(invoke_context)?;

        let transaction_context = &invoke_context.transaction_context;
        let instruction_context =
            transaction_context.get_current_instruction_context()?;
        let signers = instruction_context.get_signers()?;

        match instruction {
            ModifyAccounts { .. } => {
                composition_removed(invoke_context, "ModifyAccounts")
            }
            ScheduleCommit => process_schedule_commit(
                signers,
                invoke_context,
                ProcessScheduleCommitOptions {
                    request_undelegation: false,
                },
            ),
            ScheduleCommitAndUndelegate => process_schedule_commit(
                signers,
                invoke_context,
                ProcessScheduleCommitOptions {
                    request_undelegation: true,
                },
            ),
            Unused => {
                solana_log_collector::ic_msg!(
                    invoke_context,
                    "MagicBlockInstruction ERR: Unused instruction slot"
                );
                Err(InstructionError::InvalidInstructionData)
            }
            AcceptScheduleCommits => {
                process_accept_scheduled_commits(signers, invoke_context)
            }
            ScheduledCommitSent((id, _bump)) => process_scheduled_commit_sent(
                signers,
                invoke_context,
                transaction_context,
                id,
            ),
            ScheduleBaseIntent(args) => process_schedule_intent_bundle(
                signers,
                invoke_context,
                args.into(),
                false,
            ),
            ScheduleIntentBundle(args) => process_schedule_intent_bundle(
                signers,
                invoke_context,
                args,
                true,
            ),
            ScheduleTask(args) => {
                process_schedule_task(signers, invoke_context, args)
            }
            CancelTask { task_id } => {
                process_cancel_task(signers, invoke_context, task_id)
            }
            // NOTE: Solana runtime 3.x no longer exposes executable checks,
            // and program deployment works without them.
            DisableExecutableCheck => Ok(()),
            EnableExecutableCheck => Ok(()),
            CreateEphemeralAccount { data_len } => {
                process_create_ephemeral_account(
                    invoke_context,
                    transaction_context,
                    data_len,
                )
            }
            ResizeEphemeralAccount { new_data_len } => {
                process_resize_ephemeral_account(
                    invoke_context,
                    transaction_context,
                    new_data_len,
                )
            }
            CloseEphemeralAccount => process_close_ephemeral_account(
                invoke_context,
                transaction_context,
            ),
            AddActionCallback(args) => {
                process_add_action_callback(signers, invoke_context, args)
            }
            EvictAccount { .. } => {
                composition_removed(invoke_context, "EvictAccount")
            }
            Noop(_) => Ok(()),
            CloneAccount { .. } => {
                composition_removed(invoke_context, "CloneAccount")
            }
            CloneAccountInit { .. } => {
                composition_removed(invoke_context, "CloneAccountInit")
            }
            CloneAccountContinue { .. } => {
                composition_removed(invoke_context, "CloneAccountContinue")
            }
            CleanupPartialClone { .. } => {
                composition_removed(invoke_context, "CleanupPartialClone")
            }
            FinalizeProgramFromBuffer { .. } => {
                composition_removed(invoke_context, "FinalizeProgramFromBuffer")
            }
            SetProgramAuthority { .. } => {
                composition_removed(invoke_context, "SetProgramAuthority")
            }
            FinalizeV1ProgramFromBuffer { .. } => composition_removed(
                invoke_context,
                "FinalizeV1ProgramFromBuffer",
            ),
            ExecuteCrank {
                authority,
                instructions,
            } => process_execute_crank(
                signers,
                invoke_context,
                &authority,
                instructions,
            ),
        }
    }
);

declare_process_instruction!(
    CrankEntrypoint,
    DEFAULT_COMPUTE_UNITS,
    |invoke_context| {
        let instruction = deserialize_instruction(invoke_context)?;
        let transaction_context = &invoke_context.transaction_context;
        let instruction_context =
            transaction_context.get_current_instruction_context()?;
        let signers = instruction_context.get_signers()?;

        match instruction {
            MagicBlockInstruction::ExecuteCrank {
                authority,
                instructions,
            } => process_execute_crank(
                signers,
                invoke_context,
                &authority,
                instructions,
            ),
            _ => Err(InstructionError::InvalidInstructionData),
        }
    }
);

declare_process_instruction!(
    CallbackEntrypoint,
    DEFAULT_COMPUTE_UNITS,
    |invoke_context| {
        let instruction: CallbackInstruction =
            deserialize_instruction(invoke_context)?;
        let transaction_context = &invoke_context.transaction_context;
        let instruction_context =
            transaction_context.get_current_instruction_context()?;
        let signers = instruction_context.get_signers()?;

        match instruction {
            CallbackInstruction::ExecuteCallback { instruction } => {
                process_execute_callback(signers, invoke_context, instruction)
            }
        }
    }
);

declare_process_instruction!(
    PostDelegationActionEntrypoint,
    DEFAULT_COMPUTE_UNITS,
    |invoke_context| {
        let instruction: PostDelegationActionExecutorInstruction =
            deserialize_instruction(invoke_context)?;

        // Both variants only ever ran as a continuation of a `CloneAccount*`
        // instruction in the same transaction, which can no longer succeed.
        match instruction {
            PostDelegationActionExecutorInstruction::Execute { .. } => {
                composition_removed(
                    invoke_context,
                    "PostDelegationActionExecutor::Execute",
                )
            }
            PostDelegationActionExecutorInstruction::ScheduleUndelegation {
                ..
            } => composition_removed(
                invoke_context,
                "PostDelegationActionExecutor::ScheduleUndelegation",
            ),
        }
    }
);

declare_process_instruction!(
    EphemeralSystemEntrypoint,
    DEFAULT_COMPUTE_UNITS,
    |invoke_context| {
        let instruction: EphemeralSystemInstruction =
            deserialize_instruction(invoke_context)?;
        let transaction_context = &invoke_context.transaction_context;

        match instruction {
            EphemeralSystemInstruction::CreateEphemeralAccount { data_len } => {
                process_create_ephemeral_account(
                    invoke_context,
                    transaction_context,
                    data_len,
                )
            }
            EphemeralSystemInstruction::ResizeEphemeralAccount {
                new_data_len,
            } => process_resize_ephemeral_account(
                invoke_context,
                transaction_context,
                new_data_len,
            ),
            EphemeralSystemInstruction::CloseEphemeralAccount => {
                process_close_ephemeral_account(
                    invoke_context,
                    transaction_context,
                )
            }
        }
    }
);

#[cfg(test)]
mod test {
    use magicblock_magic_program_api::args::ScheduleTaskArgs;
    use solana_instruction::AccountMeta;
    use solana_program_runtime::{
        invoke_context::mock_process_instruction,
        solana_sbpf::program::BuiltinFunctionDefinition,
    };

    use super::*;

    #[test]
    fn crank_entrypoint_rejects_non_execute_crank_instructions() {
        let data = wincode::serialize(&MagicBlockInstruction::ScheduleTask(
            ScheduleTaskArgs {
                task_id: 1,
                execution_interval_millis: 10,
                iterations: 1,
                instructions: vec![],
            },
        ))
        .unwrap();

        mock_process_instruction(
            &crate::CRANK_PROGRAM_ID,
            None,
            &data,
            Vec::new(),
            vec![AccountMeta::new_readonly(crate::CRANK_PROGRAM_ID, false)],
            Err(InstructionError::InvalidInstructionData),
            (CrankEntrypoint::vm, CrankEntrypoint::codegen),
            |_invoke_context| {},
            |_invoke_context| {},
        );
    }

    #[test]
    fn callback_entrypoint_rejects_invalid_instruction_data() {
        let data = vec![0xFF, 0xFF, 0xFF, 0xFF];

        mock_process_instruction(
            &crate::CALLBACK_PROGRAM_ID,
            None,
            &data,
            Vec::new(),
            vec![AccountMeta::new_readonly(crate::CALLBACK_PROGRAM_ID, false)],
            Err(InstructionError::InvalidInstructionData),
            (CallbackEntrypoint::vm, CallbackEntrypoint::codegen),
            |_invoke_context| {},
            |_invoke_context| {},
        );
    }
}
