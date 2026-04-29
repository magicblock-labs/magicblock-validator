use magicblock_magic_program_api::{
    args::{
        BaseActionArgs, CommitAndUndelegateArgs, CommitTypeArgs,
        MagicIntentBundleArgs,
    },
    instruction::{AccountModificationForInstruction, MagicBlockInstruction},
};
use serde::Deserialize;
use solana_instruction::error::InstructionError;
use solana_program_runtime::declare_process_instruction;
use solana_pubkey::Pubkey;
use std::collections::HashMap;

use crate::{
    clone_account::{
        process_cleanup_partial_clone, process_clone_account,
        process_clone_account_continue, process_clone_account_init,
        process_evict_account, process_finalize_program_from_buffer,
        process_finalize_v1_program_from_buffer, process_set_program_authority,
    },
    ephemeral_accounts::{
        process_close_ephemeral_account, process_create_ephemeral_account,
        process_resize_ephemeral_account,
    },
    mutate_accounts::process_mutate_accounts,
    process_scheduled_commit_sent,
    schedule_task::{
        process_cancel_task, process_execute_crank, process_schedule_task,
    },
    schedule_transactions::{
        process_accept_scheduled_commits, process_add_action_callback,
        process_schedule_commit, process_schedule_commit_finalize,
        process_schedule_intent_bundle, ProcessScheduleCommitOptions,
    },
};

pub const DEFAULT_COMPUTE_UNITS: u64 = 150;

/// This enables the program to still be able to deserialize
/// instructions despite the changes in instructions introduced
/// by compressed commits.
#[allow(dead_code)]
#[derive(Deserialize)]
enum LegacyMagicBlockInstruction {
    ModifyAccounts {
        accounts: HashMap<Pubkey, AccountModificationForInstruction>,
        message: Option<String>,
    },
    ScheduleCommit,
    ScheduleCommitAndUndelegate,
    AcceptScheduleCommits,
    ScheduledCommitSent((u64, u64)),
    ScheduleBaseIntent(LegacyMagicBaseIntentArgs),
    ScheduleTask(magicblock_magic_program_api::args::ScheduleTaskArgs),
    CancelTask {
        task_id: i64,
    },
    DisableExecutableCheck,
    EnableExecutableCheck,
    Noop(u64),
    ScheduleIntentBundle(LegacyMagicIntentBundleArgs),
}

#[derive(Deserialize)]
enum LegacyMagicBaseIntentArgs {
    BaseActions(Vec<BaseActionArgs>),
    Commit(CommitTypeArgs),
    CommitAndUndelegate(CommitAndUndelegateArgs),
    CommitFinalize(CommitTypeArgs),
    CommitFinalizeAndUndelegate(CommitAndUndelegateArgs),
}

impl From<LegacyMagicBaseIntentArgs> for MagicIntentBundleArgs {
    fn from(value: LegacyMagicBaseIntentArgs) -> Self {
        let mut args = MagicIntentBundleArgs::default();
        match value {
            LegacyMagicBaseIntentArgs::BaseActions(actions) => {
                args.standalone_actions = actions
            }
            LegacyMagicBaseIntentArgs::Commit(commit) => {
                args.commit = Some(commit)
            }
            LegacyMagicBaseIntentArgs::CommitAndUndelegate(cau) => {
                args.commit_and_undelegate = Some(cau)
            }
            LegacyMagicBaseIntentArgs::CommitFinalize(commit) => {
                args.commit_finalize = Some(commit)
            }
            LegacyMagicBaseIntentArgs::CommitFinalizeAndUndelegate(cau) => {
                args.commit_finalize_and_undelegate = Some(cau)
            }
        }
        args
    }
}

#[derive(Deserialize)]
struct LegacyMagicIntentBundleArgs {
    commit: Option<CommitTypeArgs>,
    commit_and_undelegate: Option<CommitAndUndelegateArgs>,
    commit_finalize: Option<CommitTypeArgs>,
    commit_finalize_and_undelegate: Option<CommitAndUndelegateArgs>,
    standalone_actions: Vec<BaseActionArgs>,
}

impl From<LegacyMagicIntentBundleArgs> for MagicIntentBundleArgs {
    fn from(value: LegacyMagicIntentBundleArgs) -> Self {
        Self {
            commit: value.commit,
            commit_and_undelegate: value.commit_and_undelegate,
            commit_finalize: value.commit_finalize,
            commit_finalize_and_undelegate: value
                .commit_finalize_and_undelegate,
            commit_finalize_compressed: None,
            commit_finalize_compressed_and_undelegate: None,
            standalone_actions: value.standalone_actions,
        }
    }
}

fn deserialize_legacy_instruction(
    data: &[u8],
) -> Result<MagicBlockInstruction, InstructionError> {
    let legacy = bincode::deserialize::<LegacyMagicBlockInstruction>(data)
        .map_err(|_| InstructionError::InvalidInstructionData)?;

    match legacy {
        LegacyMagicBlockInstruction::ScheduleBaseIntent(args) => {
            Ok(MagicBlockInstruction::ScheduleIntentBundle(args.into()))
        }
        LegacyMagicBlockInstruction::ScheduleIntentBundle(args) => {
            Ok(MagicBlockInstruction::ScheduleIntentBundle(args.into()))
        }
        _ => Err(InstructionError::InvalidInstructionData),
    }
}

fn deserialize_instruction(
    invoke_context: &mut solana_program_runtime::invoke_context::InvokeContext,
) -> Result<MagicBlockInstruction, InstructionError> {
    let instruction_context = invoke_context
        .transaction_context
        .get_current_instruction_context()?;
    let data = instruction_context.get_instruction_data();

    bincode::deserialize(data)
        .or_else(|_| deserialize_legacy_instruction(data))
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
                    compressed: false,
                },
            ),
            ScheduleCommitAndUndelegate => process_schedule_commit(
                signers,
                invoke_context,
                ProcessScheduleCommitOptions {
                    request_undelegation: true,
                    compressed: false,
                },
            ),
            ScheduleCommitFinalize {
                request_undelegation,
            } => process_schedule_commit_finalize(
                signers,
                invoke_context,
                ProcessScheduleCommitOptions {
                    request_undelegation,
                    compressed: false,
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
            EvictAccount { pubkey } => process_evict_account(
                &signers,
                invoke_context,
                transaction_context,
                pubkey,
            ),
            Noop(_) => Ok(()),
            CloneAccount {
                pubkey,
                data,
                fields,
                actions_tx_sig,
            } => process_clone_account(
                &signers,
                invoke_context,
                transaction_context,
                pubkey,
                data,
                fields,
                actions_tx_sig,
            ),
            CloneAccountInit {
                pubkey,
                total_data_len,
                initial_data,
                fields,
                actions_tx_sig,
            } => process_clone_account_init(
                &signers,
                invoke_context,
                transaction_context,
                pubkey,
                total_data_len,
                initial_data,
                fields,
                actions_tx_sig,
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
            FinalizeV1ProgramFromBuffer {
                remote_slot,
                authority,
            } => process_finalize_v1_program_from_buffer(
                &signers,
                invoke_context,
                transaction_context,
                remote_slot,
                authority,
            ),
            ExecuteCrank { instructions } => {
                process_execute_crank(signers, invoke_context, instructions)
            }
            ScheduleCommitCompressed => process_schedule_commit_finalize(
                signers,
                invoke_context,
                ProcessScheduleCommitOptions {
                    request_undelegation: false,
                    compressed: true,
                },
            ),
            ScheduleCommitAndUndelegateCompressed => {
                process_schedule_commit_finalize(
                    signers,
                    invoke_context,
                    ProcessScheduleCommitOptions {
                        request_undelegation: true,
                        compressed: true,
                    },
                )
            }
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
            MagicBlockInstruction::ExecuteCrank { instructions } => {
                process_execute_crank(signers, invoke_context, instructions)
            }
            _ => Err(InstructionError::InvalidInstructionData),
        }
    }
);

#[cfg(test)]
mod test {
    use magicblock_magic_program_api::args::ScheduleTaskArgs;
    use solana_instruction::AccountMeta;
    use solana_program_runtime::invoke_context::mock_process_instruction;

    use super::*;

    #[test]
    fn crank_entrypoint_rejects_non_execute_crank_instructions() {
        let data = bincode::serialize(&MagicBlockInstruction::ScheduleTask(
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
            CrankEntrypoint::vm,
            |_invoke_context| {},
            |_invoke_context| {},
        );
    }
}
