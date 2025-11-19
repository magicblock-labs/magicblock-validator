use std::collections::HashSet;

use magicblock_magic_program_api::{
    instruction::MagicBlockInstruction, Pubkey,
};
use solana_program_runtime::{
    declare_process_instruction, invoke_context::InvokeContext,
};
use solana_sdk::{
    instruction::InstructionError, program_utils::limited_deserialize,
};

use crate::{
    mutate_accounts::process_mutate_accounts,
    process_scheduled_commit_sent,
    schedule_task::{
        process_cancel_task, process_process_tasks, process_schedule_task,
    },
    schedule_transactions::{
        process_accept_scheduled_commits, process_schedule_base_intent,
        process_schedule_commit, process_schedule_compressed_commit,
        ProcessScheduleCommitOptions,
    },
    toggle_executable_check::process_toggle_executable_check,
};

pub const DEFAULT_COMPUTE_UNITS: u64 = 150;

declare_process_instruction!(
    Entrypoint,
    DEFAULT_COMPUTE_UNITS,
    |invoke_context| {
        use MagicBlockInstruction::*;
        let instruction = limited_deserialize(
            invoke_context
                .transaction_context
                .get_current_instruction_context()?
                .get_instruction_data(),
        )?;

        let transaction_context = &invoke_context.transaction_context;
        let instruction_context =
            transaction_context.get_current_instruction_context()?;
        let signers = instruction_context.get_signers(transaction_context)?;

        match instruction {
            ModifyAccounts(mut account_mods) => process_mutate_accounts(
                signers,
                invoke_context,
                transaction_context,
                &mut account_mods,
            ),
            ScheduleCommit => {
                dispatch_commit(signers, invoke_context, false, false)
            }
            ScheduleCompressedCommit => {
                dispatch_commit(signers, invoke_context, false, true)
            }
            ScheduleCommitAndUndelegate => {
                dispatch_commit(signers, invoke_context, true, false)
            }
            ScheduleCompressedCommitAndUndelegate => {
                dispatch_commit(signers, invoke_context, true, true)
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
            ScheduleBaseIntent(args) => {
                process_schedule_base_intent(signers, invoke_context, args)
            }
            ScheduleTask(args) => {
                process_schedule_task(signers, invoke_context, args)
            }
            CancelTask { task_id } => {
                process_cancel_task(signers, invoke_context, task_id)
            }
            ProcessTasks => process_process_tasks(signers, invoke_context),
            DisableExecutableCheck => {
                process_toggle_executable_check(signers, invoke_context, false)
            }
            EnableExecutableCheck => {
                process_toggle_executable_check(signers, invoke_context, true)
            }
        }
    }
);

fn dispatch_commit(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    request_undelegation: bool,
    compressed: bool,
) -> Result<(), InstructionError> {
    let opts = ProcessScheduleCommitOptions {
        request_undelegation,
    };
    if compressed {
        process_schedule_compressed_commit(signers, invoke_context, opts)
    } else {
        process_schedule_commit(signers, invoke_context, opts)
    }
}
