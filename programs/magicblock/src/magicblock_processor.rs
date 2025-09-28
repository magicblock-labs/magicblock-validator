use magicblock_core::magic_program::instruction::MagicBlockInstruction;
use solana_program_runtime::declare_process_instruction;
use solana_sdk::program_utils::limited_deserialize;

use crate::{
    mutate_accounts::process_mutate_accounts,
    process_scheduled_commit_sent,
    schedule_transactions::{
        process_accept_scheduled_commits, process_schedule_base_intent,
        process_schedule_commit, ProcessScheduleCommitOptions,
    },
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
        let disable_executable_check = matches!(instruction, ModifyAccounts(_));
        // The below is necessary to avoid:
        // 'instruction changed executable accounts data'
        // writing data to and deploying a program account.
        // NOTE: better to make this an instruction which does nothing but toggle
        // this flag on and off around the instructions which need it off.
        if disable_executable_check {
            invoke_context
                .transaction_context
                .set_remove_accounts_executable_flag_checks(true);
        }
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
        }
    }
);
