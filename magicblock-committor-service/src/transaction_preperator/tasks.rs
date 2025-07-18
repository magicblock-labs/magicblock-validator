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
use magicblock_program::magic_scheduled_l1_message::{
    CommittedAccountV2, L1Action,
};
use solana_pubkey::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};

use crate::{
    consts::MAX_WRITE_CHUNK_SIZE,
    transaction_preperator::budget_calculator::ComputeBudgetV1,
};

pub struct TaskPreparationInfo {
    pub chunks_pda: Pubkey,
    pub buffer_pda: Pubkey,
    pub init_instruction: Instruction,
    pub realloc_instructions: Vec<Instruction>,
    pub write_instructions: Vec<Instruction>,
}

/// A trait representing a task that can be executed on Base layer
pub trait L1Task: Send + Sync {
    /// Gets all pubkeys that involved in Task's instruction
    fn involved_accounts(&self, validator: &Pubkey) -> Vec<Pubkey> {
        self.instruction(validator)
            .accounts
            .iter()
            .map(|meta| meta.pubkey)
            .collect()
    }

    /// Gets instruction for task execution
    fn instruction(&self, validator: &Pubkey) -> Instruction;

    /// If has optimizations returns optimized Task, otherwise returns itself
    fn optimize(self: Box<Self>) -> Result<Box<dyn L1Task>, Box<dyn L1Task>>;

    /// Returns [`TaskPreparationInfo`] if task needs to be prepared before executing,
    /// otherwise returns None
    fn preparation_info(
        &self,
        authority_pubkey: &Pubkey,
    ) -> Option<TaskPreparationInfo>;

    /// Returns [`Task`] budget
    fn budget(&self) -> ComputeBudgetV1;

    /// Returns Instructions per TX
    // TODO(edwin): shall be here?
    fn instructions_from_info(
        &self,
        info: &TaskPreparationInfo,
    ) -> Vec<Vec<Instruction>> {
        chunk_realloc_ixs(
            info.realloc_instructions.clone(),
            Some(info.init_instruction.clone()),
        )
    }
}

// TODO(edwin): commit_id is common thing, extract
#[derive(Clone)]
pub struct CommitTask {
    pub commit_id: u64,
    pub allow_undelegation: bool,
    pub committed_account: CommittedAccountV2,
}

#[derive(Clone)]
pub struct UndelegateTask {
    pub delegated_account: Pubkey,
    pub owner_program: Pubkey,
    pub rent_reimbursement: Pubkey,
}

#[derive(Clone)]
pub struct FinalizeTask {
    pub delegated_account: Pubkey,
}

/// Task that will be executed on Base layer via arguments
#[derive(Clone)]
pub enum ArgsTask {
    Commit(CommitTask),
    Finalize(FinalizeTask), // TODO(edwin): introduce Stages instead?
    Undelegate(UndelegateTask), // Special action really
    L1Action(L1Action),
}

impl L1Task for ArgsTask {
    fn instruction(&self, validator: &Pubkey) -> Instruction {
        match self {
            Self::Commit(value) => {
                let args = CommitStateArgs {
                    slot: value.commit_id, // TODO(edwin): change slot,
                    lamports: value.committed_account.account.lamports,
                    data: value.committed_account.account.data.clone(),
                    allow_undelegation: value.allow_undelegation, // TODO(edwin):
                };
                dlp::instruction_builder::commit_state(
                    *validator,
                    value.committed_account.pubkey,
                    value.committed_account.account.owner,
                    args,
                )
            }
            Self::Finalize(value) => dlp::instruction_builder::finalize(
                *validator,
                value.delegated_account,
            ),
            Self::Undelegate(value) => dlp::instruction_builder::undelegate(
                *validator,
                value.delegated_account,
                value.owner_program,
                value.rent_reimbursement,
            ),
            Self::L1Action(value) => {
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
                    *validator,
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
    }

    fn optimize(self: Box<Self>) -> Result<Box<dyn L1Task>, Box<dyn L1Task>> {
        match *self {
            Self::Commit(value) => Ok(Box::new(BufferTask::Commit(value))),
            Self::L1Action(_) | Self::Finalize(_) | Self::Undelegate(_) => {
                Err(self)
            }
        }
    }

    /// Nothing to prepare for [`ArgsTask`] type
    fn preparation_info(&self, _: &Pubkey) -> Option<TaskPreparationInfo> {
        None
    }

    fn budget(&self) -> ComputeBudgetV1 {
        todo!()
    }
}

/// Tasks that could be executed using buffers
#[derive(Clone)]
pub enum BufferTask {
    Commit(CommitTask),
    // TODO(edwin): Action in the future
}

impl L1Task for BufferTask {
    fn instruction(&self, validator: &Pubkey) -> Instruction {
        let Self::Commit(value) = self;
        let commit_id_slice = value.commit_id.to_le_bytes();
        let (commit_buffer_pubkey, _) =
            magicblock_committor_program::pdas::chunks_pda(
                validator,
                &value.committed_account.pubkey,
                &commit_id_slice,
            );

        dlp::instruction_builder::commit_state_from_buffer(
            *validator,
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

    /// No further optimizations
    fn optimize(self: Box<Self>) -> Result<Box<dyn L1Task>, Box<dyn L1Task>> {
        Err(self)
    }

    fn preparation_info(
        &self,
        authority_pubkey: &Pubkey,
    ) -> Option<TaskPreparationInfo> {
        let Self::Commit(commit_task) = self;

        let committed_account = &commit_task.committed_account;
        let chunks = Chunks::from_data_length(
            committed_account.account.data.len(),
            MAX_WRITE_CHUNK_SIZE,
        );
        let chunks_account_size = borsh::object_length(&chunks).unwrap() as u64;
        let buffer_account_size = committed_account.account.data.len() as u64;

        let (init_instruction, chunks_pda, buffer_pda) =
            create_init_ix(CreateInitIxArgs {
                authority: *authority_pubkey,
                pubkey: committed_account.pubkey,
                chunks_account_size,
                buffer_account_size,
                commit_id: commit_task.commit_id,
                chunk_count: chunks.count(),
                chunk_size: chunks.chunk_size(),
            });

        let realloc_instructions =
            create_realloc_buffer_ixs(CreateReallocBufferIxArgs {
                authority: *authority_pubkey,
                pubkey: committed_account.pubkey,
                buffer_account_size,
                commit_id: commit_task.commit_id,
            });

        let chunks_iter = ChangesetChunks::new(&chunks, chunks.chunk_size())
            .iter(&committed_account.account.data);
        let write_instructions = chunks_iter
            .map(|chunk| {
                create_write_ix(CreateWriteIxArgs {
                    authority: *authority_pubkey,
                    pubkey: committed_account.pubkey,
                    offset: chunk.offset,
                    data_chunk: chunk.data_chunk,
                    commit_id: commit_task.commit_id,
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

    fn budget(&self) -> ComputeBudgetV1 {
        todo!()
    }
}
