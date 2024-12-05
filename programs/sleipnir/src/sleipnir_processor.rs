use solana_program_runtime::declare_process_instruction;
use solana_sdk::program_utils::limited_deserialize;

use crate::{
    magicblock_instruction::EphemeralInstruction,
    mutate_accounts::process_mutate_accounts,
    process_scheduled_commit_sent,
    schedule_transactions::{
        process_accept_scheduled_commits, process_schedule_commit,
        ProcessScheduleCommitOptions,
    },
};

pub const DEFAULT_COMPUTE_UNITS: u64 = 150;

declare_process_instruction!(
    Entrypoint,
    DEFAULT_COMPUTE_UNITS,
    |invoke_context| {
        let transaction_context = &invoke_context.transaction_context;
        let instruction_context =
            transaction_context.get_current_instruction_context()?;
        let instruction_data = instruction_context.get_instruction_data();
        let instruction = limited_deserialize(instruction_data)?;
        let signers = instruction_context.get_signers(transaction_context)?;

        match instruction {
            EphemeralInstruction::ModifyAccounts(mut account_mods) => {
                process_mutate_accounts(
                    signers,
                    invoke_context,
                    transaction_context,
                    &mut account_mods,
                )
            }
            EphemeralInstruction::ScheduleCommit => process_schedule_commit(
                signers,
                invoke_context,
                ProcessScheduleCommitOptions {
                    request_undelegation: false,
                },
            ),
            EphemeralInstruction::ScheduleCommitAndUndelegate => {
                process_schedule_commit(
                    signers,
                    invoke_context,
                    ProcessScheduleCommitOptions {
                        request_undelegation: true,
                    },
                )
            }
            EphemeralInstruction::AcceptScheduleCommits => {
                process_accept_scheduled_commits(signers, invoke_context)
            }
            EphemeralInstruction::ScheduledCommitSent(id) => {
                process_scheduled_commit_sent(
                    signers,
                    invoke_context,
                    transaction_context,
                    id,
                )
            }
        }
    }
);
