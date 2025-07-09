use std::collections::HashMap;

use dlp::args::{CallHandlerArgs, CommitStateArgs, CommitStateFromBufferArgs};
use magicblock_committor_program::{
    instruction_builder::{
        init_buffer::{create_init_ix, CreateInitIxArgs},
        realloc_buffer::{
            create_realloc_buffer_ixs, CreateReallocBufferIxArgs,
        },
        write_buffer::{create_write_ix, CreateWriteIxArgs},
    },
    instruction_chunks::chunk_realloc_ixs,
    ChangesetChunks, Chunks,
};
use magicblock_program::magic_scheduled_l1_message::{CommitType, CommittedAccountV2, L1Action, MagicL1Message, ScheduledL1Message, UndelegateType};
use solana_pubkey::Pubkey;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    signature::Keypair,
    signer::Signer,
};

use crate::{
    consts::MAX_WRITE_CHUNK_SIZE,
    transaction_preperator::tasks::{
        ArgsTask, CommitTask, FinalizeTask, L1Task, TaskPreparationInfo,
        UndelegateTask,
    },
};

#[derive(Clone)]
pub enum Task {
    Commit(CommitTask),
    Finalize(FinalizeTask), // TODO(edwin): introduce Stages instead?
    Undelegate(UndelegateTask), // Special action really
    L1Action(L1Action),
}

impl Task {
    pub fn is_bufferable(&self) -> bool {
        match self {
            Self::Commit(_) => true,
            Self::Finalize(_) => false,
            Self::Undelegate(_) => false,
            Self::L1Action(_) => false, // TODO(edwin): enable
        }
    }

    pub fn args_instruction(&self, validator: Pubkey) -> Instruction {
        match self {
            Task::Commit(value) => {
                let args = CommitStateArgs {
                    slot: value.commit_id, // TODO(edwin): change slot,
                    lamports: value.committed_account.account.lamports,
                    data: value.committed_account.account.data.clone(),
                    allow_undelegation: value.allow_undelegation, // TODO(edwin):
                };
                dlp::instruction_builder::commit_state(
                    validator,
                    value.committed_account.pubkey,
                    value.committed_account.account.owner,
                    args,
                )
            }
            Task::Finalize(value) => dlp::instruction_builder::finalize(
                validator,
                value.delegated_account,
            ),
            Task::Undelegate(value) => dlp::instruction_builder::undelegate(
                validator,
                value.delegated_account,
                value.owner_program,
                value.rent_reimbursement,
            ),
            Task::L1Action(value) => {
                let account_metas = value
                    .account_metas_per_program
                    .iter()
                    .map(|short_meta| AccountMeta {
                        pubkey: short_meta.pubkey,
                        is_writable: short_meta.is_writable,
                        is_signer: false,
                    })
                    .collect();
                dlp::instruction_builder::call_handler(
                    validator,
                    value.destination_program,
                    value.escrow_authority,
                    account_metas,
                    CallHandlerArgs {
                        data: value.data_per_program.data.clone(),
                        escrow_index: value.data_per_program.escrow_index,
                    },
                )
            }
        }
        todo!()
    }

    pub fn buffer_instruction(&self, validator: Pubkey) -> Instruction {
        // TODO(edwin): now this is bad, while impossible
        // We should use dyn Task
        match self {
            Task::Commit(value) => {
                let commit_id_slice = value.commit_id.to_le_bytes();
                let (commit_buffer_pubkey, _) =
                    magicblock_committor_program::pdas::chunks_pda(
                        &validator,
                        &value.committed_account.pubkey,
                        &commit_id_slice,
                    );
                dlp::instruction_builder::commit_state_from_buffer(
                    validator,
                    value.committed_account.pubkey,
                    value.committed_account.account.owner,
                    commit_buffer_pubkey,
                    CommitStateFromBufferArgs {
                        slot: value.commit_id, //TODO(edwin): change to commit_id
                        lamports: value.committed_account.account.lamports,
                        allow_undelegation: value.allow_undelegation,
                    },
                )
            }
            Task::Undelegate(_) => unreachable!(),
            Task::Finalize(_) => unreachable!(),
            Task::L1Action(_) => unreachable!(), // TODO(edwin): enable
        }
    }

    pub fn get_preparation_instructions(
        &self,
        authority: &Keypair,
    ) -> Option<TaskPreparationInfo> {
        let Self::Commit(commit_task) = self else {
            None
        };

        let committed_account = &commit_task.committed_account;
        let chunks = Chunks::from_data_length(
            committed_account.account.data.len(),
            MAX_WRITE_CHUNK_SIZE,
        );
        let chunks_account_size =
            borsh::object_length(&chunks).unwrap().len() as u64;
        let buffer_account_size = committed_account.account.data.len() as u64;

        let (init_instruction, chunks_pda, buffer_pda) =
            create_init_ix(CreateInitIxArgs {
                authority: authority.pubkey(),
                pubkey: committed_account.pubkey,
                chunks_account_size,
                buffer_account_size,
                commit_id,
                chunk_count: chunks.count(),
                chunk_size: chunks.chunk_size(),
            });

        let realloc_instructions =
            create_realloc_buffer_ixs(CreateReallocBufferIxArgs {
                authority: authority.pubkey(),
                pubkey: committed_account.pubkey,
                buffer_account_size,
                commit_id,
            });

        let chunks_iter = ChangesetChunks::new(&chunks, chunks.chunk_size())
            .iter(&committed_account.account.data);
        let write_instructions = chunks_iter
            .map(|chunk| {
                create_write_ix(CreateWriteIxArgs {
                    authority: authority.pubkey(),
                    pubkey,
                    offset: chunk.offset,
                    data_chunk: chunk.data_chunk,
                    commit_id,
                })
            })
            .collect::<Vec<_>>();

        Some(TaskPreparationInfo {
            chunks_pda,
            buffer_pda,
            init_instruction,
            realloc_instructions,
            write_instructions,
        })
    }

    pub fn instructions_from_info(
        &self,
        info: &TaskPreparationInfo,
    ) -> Vec<Vec<Instruction>> {
        chunk_realloc_ixs(
            info.realloc_instructions.clone(),
            Some(info.init_instruction.clone()),
        )
    }
}

pub trait TasksBuilder {
    // Creates tasks for commit stage
    fn commit_tasks(
        l1_message: &ScheduledL1Message,
        commit_ids: HashMap<Pubkey, u64>,
    ) -> Vec<Box<dyn L1Task>>;

    // Create tasks for finalize stage
    fn finalize_tasks(
        l1_message: &ScheduledL1Message,
        rent_reimbursement: &Pubkey,
    ) -> Vec<Box<dyn L1Task>>;
}

/// V1 Task builder
/// V1: Actions are part of finalize tx
pub struct TaskBuilderV1;
impl TasksBuilder for TaskBuilderV1 {
    /// Returns [`Task`]s for Commit stage
    fn commit_tasks(
        l1_message: &ScheduledL1Message,
        commit_ids: HashMap<Pubkey, u64>,
    ) -> Vec<Box<dyn L1Task>> {
        let (accounts, allow_undelegation) = match &l1_message.l1_message {
            MagicL1Message::L1Actions(actions) => {
                return actions
                    .into_iter()
                    .map(|el| Task::L1Action(el.clone()))
                    .collect()
            }
            MagicL1Message::Commit(t) => (t.get_committed_accounts(), false),
            MagicL1Message::CommitAndUndelegate(t) => {
                (t.commit_action.get_committed_accounts(), true)
            }
        };

        accounts
            .into_iter()
            .map(|account| {
                if let Some(commit_id) = commit_ids.get(&account.pubkey) {
                    Ok(ArgsTask::Commit(CommitTask {
                        commit_id: *commit_id + 1,
                        allow_undelegation,
                        committed_account: account.clone(),
                    }))
                } else {
                    // TODO(edwin): proper error
                    Err(())
                }
            })
            .collect::<Result<_, _>>()
            .unwrap() // TODO(edwin): remove
    }

    /// Returns [`Task`]s for Finalize stage
    fn finalize_tasks(
        l1_message: &ScheduledL1Message,
        rent_reimbursement: &Pubkey,
    ) -> Vec<Box<dyn L1Task>> {
        // Helper to create a finalize task
        fn finalize_task(account: &CommittedAccountV2) -> Box<dyn L1Task> {
            Box::new(ArgsTask::Finalize(FinalizeTask {
                delegated_account: account.pubkey,
            }))
        }

        // Helper to create an undelegate task
        fn undelegate_task(account: &CommittedAccountV2, rent_reimbursement: &Pubkey) -> Box<dyn L1Task> {
            Box::new(ArgsTask::Undelegate(UndelegateTask {
                delegated_account: account.pubkey,
                owner_program: account.account.owner,
                rent_reimbursement: *rent_reimbursement,
            }))
        }

        // Helper to process commit types
        fn process_commit(commit: &CommitType) -> Vec<Box<dyn L1Task>> {
            match commit {
                CommitType::Standalone(accounts) => accounts.iter().map(finalize_task).collect(),
                CommitType::WithL1Actions { committed_accounts, l1_actions } => {
                    let mut tasks = committed_accounts.iter().map(finalize_task).collect::<Vec<_>>();
                    tasks.extend(l1_actions.iter().map(|a| Box::new(ArgsTask::L1Action(a.clone()))));
                    tasks
                }
            }
        }

        match &l1_message.l1_message {
            MagicL1Message::L1Actions(_) => vec![],
            MagicL1Message::Commit(commit) => process_commit(commit),
            MagicL1Message::CommitAndUndelegate(t) => {
                let mut tasks = process_commit(&t.commit_action);
                let accounts = t.get_committed_accounts();

                match &t.undelegate_action {
                    UndelegateType::Standalone => {
                        tasks.extend(accounts.iter().map(|a| undelegate_task(a, rent_reimbursement)));
                    }
                    UndelegateType::WithL1Actions(actions) => {
                        tasks.extend(accounts.iter().map(|a| undelegate_task(a, rent_reimbursement)));
                        tasks.extend(actions.iter().map(|a| Box::new(ArgsTask::L1Action(a.clone()))));
                    }
                }

                tasks
            }
        }
    }
}
