use std::collections::HashSet;

use magicblock_magic_program_api::instruction::MagicBlockInstruction;
use solana_instruction::error::InstructionError;
use solana_program_runtime::{
    declare_process_instruction, invoke_context::InvokeContext,
};
use solana_pubkey::Pubkey;

use crate::{
    mutate_accounts::process_mutate_accounts,
    process_scheduled_commit_sent,
    schedule_task::{process_cancel_task, process_schedule_task},
    schedule_transactions::{
        process_accept_scheduled_commits, process_schedule_base_intent,
        process_schedule_commit, process_schedule_compressed_commit,
        ProcessScheduleCommitOptions,
    },
    toggle_executable_check::process_toggle_executable_check,
};

pub const DEFAULT_COMPUTE_UNITS: u64 = 150;

pub enum CommitKind {
    Commit,
    CommitAndUndelegate,
    CompressedCommit,
    CompressedCommitAndUndelegate,
}

impl CommitKind {
    pub fn request_undelegation(&self) -> bool {
        matches!(
            self,
            CommitKind::CommitAndUndelegate
                | CommitKind::CompressedCommitAndUndelegate
        )
    }

    pub fn compressed(&self) -> bool {
        matches!(
            self,
            CommitKind::CompressedCommit
                | CommitKind::CompressedCommitAndUndelegate
        )
    }
}

declare_process_instruction!(
    Entrypoint,
    DEFAULT_COMPUTE_UNITS,
    |invoke_context| {
        use MagicBlockInstruction::*;
        let instruction: MagicBlockInstruction = bincode::deserialize(
            invoke_context
                .transaction_context
                .get_current_instruction_context()?
                .get_instruction_data(),
        )
        .map_err(|_| {
            solana_instruction::error::InstructionError::InvalidInstructionData
        })?;

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
                dispatch_commit(signers, invoke_context, CommitKind::Commit)
            }
            ScheduleCompressedCommit => dispatch_commit(
                signers,
                invoke_context,
                CommitKind::CompressedCommit,
            ),
            ScheduleCommitAndUndelegate => dispatch_commit(
                signers,
                invoke_context,
                CommitKind::CommitAndUndelegate,
            ),
            ScheduleCompressedCommitAndUndelegate => dispatch_commit(
                signers,
                invoke_context,
                CommitKind::CompressedCommitAndUndelegate,
            ),
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
            DisableExecutableCheck => {
                process_toggle_executable_check(signers, invoke_context, false)
            }
            EnableExecutableCheck => {
                process_toggle_executable_check(signers, invoke_context, true)
            }
            Noop(_) => Ok(()),
        }
    }
);

fn dispatch_commit(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    commit_kind: CommitKind,
) -> Result<(), InstructionError> {
    let opts = ProcessScheduleCommitOptions {
        request_undelegation: commit_kind.request_undelegation(),
    };
    if commit_kind.compressed() {
        process_schedule_compressed_commit(signers, invoke_context, opts)
    } else {
        process_schedule_commit(signers, invoke_context, opts)
    }
}
