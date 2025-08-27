use dlp::args::{
    CallHandlerArgs, CommitStateArgs, CommitStateFromBufferArgs, Context,
};
use dyn_clone::DynClone;
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskType {
    Commit,
    Finalize,
    Undelegate,
    Action,
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
pub trait BaseTask: Send + Sync + DynClone {
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

    /// Returns [`TaskType`]
    fn task_type(&self) -> TaskType;

    /// Calls [`Visitor`] with specific task type
    fn visit(&self, visitor: &mut dyn Visitor);
}

dyn_clone::clone_trait_object!(BaseTask);

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

#[derive(Clone)]
pub struct BaseActionTask {
    pub context: Context,
    pub action: BaseAction,
}

/// Task that will be executed on Base layer via arguments
#[derive(Clone)]
pub enum ArgsTask {
    Commit(CommitTask),
    Finalize(FinalizeTask),
    Undelegate(UndelegateTask), // Special action really
    BaseAction(BaseActionTask),
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
            Self::BaseAction(value) => {
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
            Self::BaseAction(_) | Self::Finalize(_) | Self::Undelegate(_) => {
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
            Self::BaseAction(task) => task.action.compute_units,
            Self::Undelegate(_) => 50_000,
            Self::Finalize(_) => 40_000,
        }
    }

    fn strategy(&self) -> TaskStrategy {
        TaskStrategy::Args
    }

    fn task_type(&self) -> TaskType {
        match self {
            Self::Commit(_) => TaskType::Commit,
            Self::BaseAction(_) => TaskType::Action,
            Self::Undelegate(_) => TaskType::Undelegate,
            Self::Finalize(_) => TaskType::Finalize,
        }
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

        // SAFETY: as object_length internally uses only already allocated or static buffers,
        // and we don't use any fs writers, so the only error that may occur here is of kind
        // OutOfMemory or WriteZero. This is impossible due to:
        // Chunks::new panics if its size exceeds MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE or 10_240
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

    fn task_type(&self) -> TaskType {
        match self {
            Self::Commit(_) => TaskType::Commit,
        }
    }

    /// For tasks using Args strategy call corresponding `Visitor` method
    fn visit(&self, visitor: &mut dyn Visitor) {
        visitor.visit_buffer_task(self);
    }
}

#[cfg(test)]
mod serialization_safety_test {
    use magicblock_program::magic_scheduled_base_intent::{
        ProgramArgs, ShortAccountMeta,
    };
    use solana_account::Account;

    use super::*;

    // Test all ArgsTask variants
    #[test]
    fn test_args_task_instruction_serialization() {
        let validator = Pubkey::new_unique();

        // Test Commit variant
        let commit_task = ArgsTask::Commit(CommitTask {
            commit_id: 123,
            allow_undelegation: true,
            committed_account: CommittedAccountV2 {
                pubkey: Pubkey::new_unique(),
                account: Account {
                    lamports: 1000,
                    data: vec![1, 2, 3],
                    owner: Pubkey::new_unique(),
                    executable: false,
                    rent_epoch: 0,
                },
            },
        });
        assert_serializable(&commit_task.instruction(&validator));

        // Test Finalize variant
        let finalize_task = ArgsTask::Finalize(FinalizeTask {
            delegated_account: Pubkey::new_unique(),
        });
        assert_serializable(&finalize_task.instruction(&validator));

        // Test Undelegate variant
        let undelegate_task = ArgsTask::Undelegate(UndelegateTask {
            delegated_account: Pubkey::new_unique(),
            owner_program: Pubkey::new_unique(),
            rent_reimbursement: Pubkey::new_unique(),
        });
        assert_serializable(&undelegate_task.instruction(&validator));

        // Test BaseAction variant
        let base_action = ArgsTask::BaseAction(BaseActionTask {
            context: Context::Undelegate,
            action: BaseAction {
                destination_program: Pubkey::new_unique(),
                escrow_authority: Pubkey::new_unique(),
                account_metas_per_program: vec![ShortAccountMeta {
                    pubkey: Pubkey::new_unique(),
                    is_writable: true,
                }],
                data_per_program: ProgramArgs {
                    data: vec![4, 5, 6],
                    escrow_index: 1,
                },
                compute_units: 10_000,
            },
        });
        assert_serializable(&base_action.instruction(&validator));
    }

    // Test BufferTask variants
    #[test]
    fn test_buffer_task_instruction_serialization() {
        let validator = Pubkey::new_unique();

        let buffer_task = BufferTask::Commit(CommitTask {
            commit_id: 456,
            allow_undelegation: false,
            committed_account: CommittedAccountV2 {
                pubkey: Pubkey::new_unique(),
                account: Account {
                    lamports: 2000,
                    data: vec![7, 8, 9],
                    owner: Pubkey::new_unique(),
                    executable: false,
                    rent_epoch: 0,
                },
            },
        });
        assert_serializable(&buffer_task.instruction(&validator));
    }

    // Test preparation instructions
    #[test]
    fn test_preparation_instructions_serialization() {
        let authority = Pubkey::new_unique();

        // Test BufferTask preparation
        let buffer_task = BufferTask::Commit(CommitTask {
            commit_id: 789,
            allow_undelegation: true,
            committed_account: CommittedAccountV2 {
                pubkey: Pubkey::new_unique(),
                account: Account {
                    lamports: 3000,
                    data: vec![0; 1024], // Larger data to test chunking
                    owner: Pubkey::new_unique(),
                    executable: false,
                    rent_epoch: 0,
                },
            },
        });

        let prep_info = buffer_task.preparation_info(&authority).unwrap();
        assert_serializable(&prep_info.init_instruction);
        for ix in prep_info.realloc_instructions {
            assert_serializable(&ix);
        }
        for ix in prep_info.write_instructions {
            assert_serializable(&ix);
        }
    }

    // Helper function to assert serialization succeeds
    fn assert_serializable(ix: &Instruction) {
        bincode::serialize(ix).unwrap_or_else(|e| {
            panic!("Failed to serialize instruction {:?}: {}", ix, e)
        });
    }
}
