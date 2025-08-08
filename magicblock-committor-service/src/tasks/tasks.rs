use dlp::args::{
    CallHandlerArgs, CommitStateArgs, CommitStateFromBufferArgs, Context,
};
use magicblock_committor_program::{
    instruction_builder::{
        init_buffer::{create_init_ix, CreateInitIxArgs},
        realloc_buffer::{
            create_realloc_buffer_ixs, CreateReallocBufferIxArgs,
        },
        write_buffer::{create_write_ix, CreateWriteIxArgs},
    },
    ChangesetChunks, Chunks,
};
use magicblock_program::magic_scheduled_base_intent::{
    BaseAction, CommittedAccountV2,
};
use solana_pubkey::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};

use crate::{consts::MAX_WRITE_CHUNK_SIZE, tasks::visitor::Visitor};

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TaskStrategy {
    Args,
    Buffer,
}

#[derive(Clone, Debug)]
pub struct TaskPreparationInfo {
    pub commit_id: u64,
    pub pubkey: Pubkey,
    pub chunks_pda: Pubkey,
    pub buffer_pda: Pubkey,
    pub init_instruction: Instruction,
    pub realloc_instructions: Vec<Instruction>,
    pub write_instructions: Vec<Instruction>,
}

/// A trait representing a task that can be executed on Base layer
pub trait BaseTask: Send + Sync {
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

    /// Optimizes Task strategy if possible, otherwise returns itself
    fn optimize(
        self: Box<Self>,
    ) -> Result<Box<dyn BaseTask>, Box<dyn BaseTask>>;

    /// Returns [`TaskPreparationInfo`] if task needs to be prepared before executing,
    /// otherwise returns None
    fn preparation_info(
        &self,
        authority_pubkey: &Pubkey,
    ) -> Option<TaskPreparationInfo>;

    /// Returns [`Task`] budget
    fn compute_units(&self) -> u32;

    /// Returns current [`TaskStrategy`]
    fn strategy(&self) -> TaskStrategy;

    /// Calls [`Visitor`] with specific task type
    fn visit(&self, visitor: &mut dyn Visitor);
}

#[derive(Clone)]
pub struct CommitTask {
    // TODO: rename to commit_nonce?
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

#[derive(Clone)]
pub struct L1ActionTask {
    pub context: Context,
    pub action: BaseAction,
}

/// Task that will be executed on Base layer via arguments
#[derive(Clone)]
pub enum ArgsTask {
    Commit(CommitTask),
    Finalize(FinalizeTask),
    Undelegate(UndelegateTask), // Special action really
    L1Action(L1ActionTask),
}

impl BaseTask for ArgsTask {
    fn instruction(&self, validator: &Pubkey) -> Instruction {
        match self {
            Self::Commit(value) => {
                let args = CommitStateArgs {
                    slot: value.commit_id,
                    lamports: value.committed_account.account.lamports,
                    data: value.committed_account.account.data.clone(),
                    allow_undelegation: value.allow_undelegation,
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
                let action = &value.action;
                let account_metas = action
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
                    action.destination_program,
                    action.escrow_authority,
                    account_metas,
                    CallHandlerArgs {
                        context: value.context,
                        data: action.data_per_program.data.clone(),
                        escrow_index: action.data_per_program.escrow_index,
                    },
                )
            }
        }
    }

    fn optimize(
        self: Box<Self>,
    ) -> Result<Box<dyn BaseTask>, Box<dyn BaseTask>> {
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

    fn compute_units(&self) -> u32 {
        match self {
            Self::Commit(_) => 55_000,
            Self::L1Action(task) => task.action.compute_units,
            Self::Undelegate(_) => 50_000,
            Self::Finalize(_) => 40_000,
        }
    }

    fn strategy(&self) -> TaskStrategy {
        TaskStrategy::Args
    }

    /// For tasks using Args strategy call corresponding `Visitor` method
    fn visit(&self, visitor: &mut dyn Visitor) {
        visitor.visit_args_task(self);
    }
}

/// Tasks that could be executed using buffers
#[derive(Clone)]
pub enum BufferTask {
    Commit(CommitTask),
    // Action in the future
}

impl BaseTask for BufferTask {
    fn instruction(&self, validator: &Pubkey) -> Instruction {
        let Self::Commit(value) = self;
        let commit_id_slice = value.commit_id.to_le_bytes();
        let (commit_buffer_pubkey, _) =
            magicblock_committor_program::pdas::buffer_pda(
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
                slot: value.commit_id,
                lamports: value.committed_account.account.lamports,
                allow_undelegation: value.allow_undelegation,
            },
        )
    }

    /// No further optimizations
    fn optimize(
        self: Box<Self>,
    ) -> Result<Box<dyn BaseTask>, Box<dyn BaseTask>> {
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
            commit_id: commit_task.commit_id,
            pubkey: commit_task.committed_account.pubkey,
            chunks_pda,
            buffer_pda,
            init_instruction,
            realloc_instructions,
            write_instructions,
        })
    }

    fn compute_units(&self) -> u32 {
        match self {
            Self::Commit(_) => 55_000,
        }
    }

    fn strategy(&self) -> TaskStrategy {
        TaskStrategy::Buffer
    }

    /// For tasks using Args strategy call corresponding `Visitor` method
    fn visit(&self, visitor: &mut dyn Visitor) {
        visitor.visit_buffer_task(self);
    }
}
