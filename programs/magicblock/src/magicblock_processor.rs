use magicblock_magic_program_api::instruction::MagicBlockInstruction;
use solana_program_runtime::declare_process_instruction;

use crate::{
    clone_account::{
        process_cleanup_partial_clone, process_clone_account,
        process_clone_account_continue, process_clone_account_init,
        process_finalize_program_from_buffer,
        process_finalize_v1_program_from_buffer, process_set_program_authority,
    },
    ephemeral_accounts::{
        process_close_ephemeral_account, process_create_ephemeral_account,
        process_resize_ephemeral_account,
    },
    mutate_accounts::process_mutate_accounts,
    process_scheduled_commit_sent,
    schedule_task::{process_cancel_task, process_schedule_task},
    schedule_transactions::{
        process_accept_scheduled_commits, process_schedule_commit,
        process_schedule_intent_bundle, ProcessScheduleCommitOptions,
    },
    toggle_executable_check::process_toggle_executable_check,
};

pub const DEFAULT_COMPUTE_UNITS: u64 = 150;

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
            ModifyAccounts {
                mut accounts,
                message,
            } => process_mutate_accounts(
                signers,
                invoke_context,
                transaction_context,
                &mut accounts,
                message,
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
            ScheduleCommitFinalize {
                request_undelegation: _,
            } => todo!(),
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
            DisableExecutableCheck => {
                process_toggle_executable_check(signers, invoke_context, false)
            }
            EnableExecutableCheck => {
                process_toggle_executable_check(signers, invoke_context, true)
            }
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
            Noop(_) => Ok(()),
            CloneAccount {
                pubkey,
                data,
                fields,
            } => process_clone_account(
                &signers,
                invoke_context,
                transaction_context,
                pubkey,
                data,
                fields,
            ),
            CloneAccountInit {
                pubkey,
                total_data_len,
                initial_data,
                fields,
            } => process_clone_account_init(
                &signers,
                invoke_context,
                transaction_context,
                pubkey,
                total_data_len,
                initial_data,
                fields,
            ),
            CloneAccountContinue {
                pubkey,
                offset,
                data,
                is_last,
            } => process_clone_account_continue(
                &signers,
                invoke_context,
                transaction_context,
                pubkey,
                offset,
                data,
                is_last,
            ),
            CleanupPartialClone { pubkey } => process_cleanup_partial_clone(
                &signers,
                invoke_context,
                transaction_context,
                pubkey,
            ),
            FinalizeProgramFromBuffer { remote_slot } => {
                process_finalize_program_from_buffer(
                    &signers,
                    invoke_context,
                    transaction_context,
                    remote_slot,
                )
            }
            SetProgramAuthority { authority } => process_set_program_authority(
                &signers,
                invoke_context,
                transaction_context,
                authority,
            ),
            FinalizeV1ProgramFromBuffer { remote_slot, authority } => {
                process_finalize_v1_program_from_buffer(
                    &signers,
                    invoke_context,
                    transaction_context,
                    remote_slot,
                    authority,
                )
            }
        }
    }
);
