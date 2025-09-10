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
    pdas, ChangesetChunks, Chunks,
};
use magicblock_program::magic_scheduled_base_intent::{
    BaseAction, CommittedAccount,
};
use solana_pubkey::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};
use thiserror::Error;

use crate::{consts::MAX_WRITE_CHUNK_SIZE, tasks::visitor::Visitor};

pub mod task_builder;
pub mod task_strategist;
pub(crate) mod task_visitors;
pub mod utils;
pub mod visitor;

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

#[derive(Clone)]
pub enum PreparationState {
    NotNeeded,
    Required(PreparationTask),
    Cleanup,
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

    /// Returns [`PreparationTask`] if task needs to be prepared before executing,
    /// otherwise returns None
    fn preparation_state(&self) -> &PreparationState;

    /// Switched [`PreparationTask`] to a new one
    fn switch_preparation_state(
        &mut self,
        new_state: PreparationState,
    ) -> BaseTaskResult<()>;

    /// Returns [`Task`] budget
    fn compute_units(&self) -> u32;

    /// Returns current [`TaskStrategy`]
    fn strategy(&self) -> TaskStrategy;

    /// Returns [`TaskType`]
    fn task_type(&self) -> TaskType;

    /// Calls [`Visitor`] with specific task type
    fn visit(&self, visitor: &mut dyn Visitor);

    /// Resets commit id
    fn reset_commit_id(&mut self, commit_id: u64);
}

dyn_clone::clone_trait_object!(BaseTask);

#[derive(Clone)]
pub struct CommitTask {
    pub commit_id: u64,
    pub allow_undelegation: bool,
    pub committed_account: CommittedAccount,
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
pub enum ArgsTaskType {
    Commit(CommitTask),
    Finalize(FinalizeTask),
    Undelegate(UndelegateTask), // Special action really
    BaseAction(BaseActionTask),
}

#[derive(Clone)]
pub struct ArgsTask {
    preparation_state: PreparationState,
    pub task_type: ArgsTaskType,
}

impl ArgsTask {
    pub fn new(task_type: ArgsTaskType) -> Self {
        Self {
            preparation_state: PreparationState::NotNeeded,
            task_type,
        }
    }
}

impl BaseTask for ArgsTask {
    fn instruction(&self, validator: &Pubkey) -> Instruction {
        match &self.task_type {
            ArgsTaskType::Commit(value) => {
                let args = CommitStateArgs {
                    nonce: value.commit_id,
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
            ArgsTaskType::Finalize(value) => {
                dlp::instruction_builder::finalize(
                    *validator,
                    value.delegated_account,
                )
            }
            ArgsTaskType::Undelegate(value) => {
                dlp::instruction_builder::undelegate(
                    *validator,
                    value.delegated_account,
                    value.owner_program,
                    value.rent_reimbursement,
                )
            }
            ArgsTaskType::BaseAction(value) => {
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
            ArgsTaskType::Commit(value) => {
                Ok(Box::new(BufferTask::new_preparation_required(
                    BufferTaskType::Commit(value),
                )))
            }
            ArgsTaskType::BaseAction(_)
            | ArgsTaskType::Finalize(_)
            | ArgsTaskType::Undelegate(_) => Err(self),
        }
    }

    /// Nothing to prepare for [`ArgsTaskType`] type
    fn preparation_state(&self) -> &PreparationState {
        &self.preparation_state
    }

    fn switch_preparation_state(
        &mut self,
        new_state: PreparationState,
    ) -> BaseTaskResult<()> {
        if !matches!(new_state, PreparationState::NotNeeded) {
            Err(BaseTaskError::PreparationStateTransitionError)
        } else {
            Ok(())
        }
    }

    fn compute_units(&self) -> u32 {
        match &self.task_type {
            ArgsTaskType::Commit(_) => 65_000,
            ArgsTaskType::BaseAction(task) => task.action.compute_units,
            ArgsTaskType::Undelegate(_) => 50_000,
            ArgsTaskType::Finalize(_) => 40_000,
        }
    }

    fn strategy(&self) -> TaskStrategy {
        TaskStrategy::Args
    }

    fn task_type(&self) -> TaskType {
        match &self.task_type {
            ArgsTaskType::Commit(_) => TaskType::Commit,
            ArgsTaskType::BaseAction(_) => TaskType::Action,
            ArgsTaskType::Undelegate(_) => TaskType::Undelegate,
            ArgsTaskType::Finalize(_) => TaskType::Finalize,
        }
    }

    /// For tasks using Args strategy call corresponding `Visitor` method
    fn visit(&self, visitor: &mut dyn Visitor) {
        visitor.visit_args_task(self);
    }

    fn reset_commit_id(&mut self, commit_id: u64) {
        let ArgsTaskType::Commit(commit_task) = &mut self.task_type else {
            return;
        };

        *commit_task.commit_id = commit_id;
    }
}

/// Tasks that could be executed using buffers
#[derive(Clone)]
pub enum BufferTaskType {
    Commit(CommitTask),
    // Action in the future
}

#[derive(Clone)]
pub struct BufferTask {
    preparation_state: PreparationState,
    pub task_type: BufferTaskType,
}

impl BufferTask {
    pub fn new_preparation_required(task_type: BufferTaskType) -> Self {
        Self {
            preparation_state: Self::preparation_required(&task_type),
            task_type,
        }
    }

    pub fn new(
        preparation_state: PreparationState,
        task_type: BufferTaskType,
    ) -> Self {
        Self {
            preparation_state,
            task_type,
        }
    }

    fn preparation_required(task_type: &BufferTaskType) -> PreparationState {
        let BufferTaskType::Commit(ref commit_task) = task_type;
        let committed_data = commit_task.committed_account.account.data.clone();
        let chunks = Chunks::from_data_length(
            committed_data.len(),
            MAX_WRITE_CHUNK_SIZE,
        );
        let preparation_state = PreparationState::Required(PreparationTask {
            commit_id: commit_task.commit_id,
            pubkey: commit_task.committed_account.pubkey,
            committed_data,
            chunks,
        });

        preparation_state
    }
}

impl BaseTask for BufferTask {
    fn instruction(&self, validator: &Pubkey) -> Instruction {
        let BufferTaskType::Commit(ref value) = self.task_type;
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
                nonce: value.commit_id,
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

    fn preparation_state(&self) -> &PreparationState {
        &self.preparation_state
    }

    fn switch_preparation_state(
        &mut self,
        new_state: PreparationState,
    ) -> BaseTaskResult<()> {
        if matches!(new_state, PreparationState::NotNeeded) {
            Err(BaseTaskError::PreparationStateTransitionError)
        } else {
            *self.preparation_state = new_state;
            Ok(())
        }
    }

    fn compute_units(&self) -> u32 {
        match self.task_type {
            BufferTaskType::Commit(_) => 65_000,
        }
    }

    fn strategy(&self) -> TaskStrategy {
        TaskStrategy::Buffer
    }

    fn task_type(&self) -> TaskType {
        match self.task_type {
            BufferTaskType::Commit(_) => TaskType::Commit,
        }
    }

    /// For tasks using Args strategy call corresponding `Visitor` method
    fn visit(&self, visitor: &mut dyn Visitor) {
        visitor.visit_buffer_task(self);
    }

    fn reset_commit_id(&mut self, commit_id: u64) {
        let BufferTaskType::Commit(commit_task) = &mut self.task_type;
        if commit_id == commit_task.commit_id {
            return;
        }

        commit_task.commit_id = commit_id;
        self.preparation_state = Self::preparation_required(&self.task_type)
    }
}

#[derive(Clone, Debug)]
pub struct PreparationTask {
    pub commit_id: u64,
    pub pubkey: Pubkey,
    pub chunks: Chunks,

    // TODO(edwin): replace with reference once done
    pub committed_data: Vec<u8>,
}

impl PreparationTask {
    /// Returns initialization [`Instruction`]
    pub fn init_instruction(&self, authority: &Pubkey) -> Instruction {
        // // SAFETY: as object_length internally uses only already allocated or static buffers,
        // // and we don't use any fs writers, so the only error that may occur here is of kind
        // // OutOfMemory or WriteZero. This is impossible due to:
        // // Chunks::new panics if its size exceeds MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE or 10_240
        // // https://github.com/near/borsh-rs/blob/f1b75a6b50740bfb6231b7d0b1bd93ea58ca5452/borsh/src/ser/helpers.rs#L59
        let chunks_account_size =
            borsh::object_length(&self.chunks).unwrap() as u64;
        let buffer_account_size = self.committed_data.len() as u64;

        let (instruction, _, _) = create_init_ix(CreateInitIxArgs {
            authority: *authority,
            pubkey: self.pubkey,
            chunks_account_size,
            buffer_account_size,
            commit_id: self.commit_id,
            chunk_count: self.chunks.count(),
            chunk_size: self.chunks.chunk_size(),
        });

        instruction
    }

    /// Returns compute units required for realloc instruction
    pub fn init_compute_units(&self) -> u32 {
        12_000
    }

    /// Returns realloc instruction required for Buffer preparation
    pub fn realloc_instructions(&self, authority: &Pubkey) -> Vec<Instruction> {
        let buffer_account_size = self.committed_data.len() as u64;
        let realloc_instructions =
            create_realloc_buffer_ixs(CreateReallocBufferIxArgs {
                authority: *authority,
                pubkey: self.pubkey,
                buffer_account_size,
                commit_id: self.commit_id,
            });

        realloc_instructions
    }

    /// Returns compute units required for realloc instruction
    pub fn realloc_compute_units(&self) -> u32 {
        6_000
    }

    /// Returns realloc instruction required for Buffer preparation
    pub fn write_instructions(&self, authority: &Pubkey) -> Vec<Instruction> {
        let chunks_iter =
            ChangesetChunks::new(&self.chunks, self.chunks.chunk_size())
                .iter(&self.committed_data);
        let write_instructions = chunks_iter
            .map(|chunk| {
                create_write_ix(CreateWriteIxArgs {
                    authority: *authority,
                    pubkey: self.pubkey,
                    offset: chunk.offset,
                    data_chunk: chunk.data_chunk,
                    commit_id: self.commit_id,
                })
            })
            .collect::<Vec<_>>();

        write_instructions
    }

    pub fn write_compute_units(&self, bytes_count: usize) -> u32 {
        const PER_BYTE: u32 = 3;

        u32::try_from(bytes_count)
            .ok()
            .and_then(|bytes_count| bytes_count.checked_mul(PER_BYTE))
            .unwrap_or(u32::MAX)
    }

    pub fn chunks_pda(&self, authority: &Pubkey) -> Pubkey {
        pdas::chunks_pda(
            authority,
            &self.pubkey,
            self.commit_id.to_le_bytes().as_slice(),
        )
        .0
    }

    pub fn buffer_pda(&self, authority: &Pubkey) -> Pubkey {
        pdas::buffer_pda(
            authority,
            &self.pubkey,
            self.commit_id.to_le_bytes().as_slice(),
        )
        .0
    }
}

#[derive(Error, Debug)]
pub enum BaseTaskError {
    #[error("Invalid preparation state transition")]
    PreparationStateTransitionError,
}

pub type BaseTaskResult<T> = Result<T, BaseTaskError>;

#[cfg(test)]
mod serialization_safety_test {
    use magicblock_program::magic_scheduled_base_intent::{
        ProgramArgs, ShortAccountMeta,
    };
    use solana_account::Account;

    use crate::tasks::*;

    // Test all ArgsTask variants
    #[test]
    fn test_args_task_instruction_serialization() {
        let validator = Pubkey::new_unique();

        // Test Commit variant
        let commit_task = ArgsTaskType::Commit(CommitTask {
            commit_id: 123,
            allow_undelegation: true,
            committed_account: CommittedAccount {
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
        let finalize_task = ArgsTaskType::Finalize(FinalizeTask {
            delegated_account: Pubkey::new_unique(),
        });
        assert_serializable(&finalize_task.instruction(&validator));

        // Test Undelegate variant
        let undelegate_task = ArgsTaskType::Undelegate(UndelegateTask {
            delegated_account: Pubkey::new_unique(),
            owner_program: Pubkey::new_unique(),
            rent_reimbursement: Pubkey::new_unique(),
        });
        assert_serializable(&undelegate_task.instruction(&validator));

        // Test BaseAction variant
        let base_action = ArgsTaskType::BaseAction(BaseActionTask {
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
            committed_account: CommittedAccount {
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
            committed_account: CommittedAccount {
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
