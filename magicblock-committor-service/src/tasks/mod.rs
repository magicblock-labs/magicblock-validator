use dlp::args::Context;
use dyn_clone::DynClone;
use magicblock_committor_program::{
    instruction_builder::{
        close_buffer::{create_close_ix, CreateCloseIxArgs},
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
use solana_sdk::instruction::Instruction;
use thiserror::Error;

use crate::tasks::visitor::Visitor;

mod args_task;
mod buffer_task;
pub mod task_builder;
pub mod task_strategist;
pub(crate) mod task_visitors;
pub mod utils;
pub mod visitor;

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
    Cleanup(CleanupTask),
}

#[cfg(test)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TaskStrategy {
    Args,
    Buffer,
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
    #[cfg(test)]
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

    pub fn cleanup_task(&self) -> CleanupTask {
        CleanupTask {
            pubkey: self.pubkey,
            commit_id: self.commit_id,
        }
    }
}

#[derive(Clone)]
pub struct CleanupTask {
    pub pubkey: Pubkey,
    pub commit_id: u64,
}

impl CleanupTask {
    pub fn instruction(&self, authority: &Pubkey) -> Instruction {
        create_close_ix(CreateCloseIxArgs {
            authority: *authority,
            pubkey: self.pubkey,
            commit_id: self.commit_id,
        })
    }

    /// Returns compute units required to execute [`CleanupTask`]
    pub fn compute_units(&self) -> u32 {
        30_000
    }

    /// Returns a number of [`CleanupTask`]s that is possible to fit in single
    pub const fn max_tx_fit_count_with_budget() -> usize {
        19
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

    use crate::tasks::{
        args_task::{ArgsTask, ArgsTaskType},
        buffer_task::{BufferTask, BufferTaskType},
        *,
    };

    // Test all ArgsTask variants
    #[test]
    fn test_args_task_instruction_serialization() {
        let validator = Pubkey::new_unique();

        // Test Commit variant
        let commit_task: ArgsTask = ArgsTaskType::Commit(CommitTask {
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
        })
        .into();
        assert_serializable(&commit_task.instruction(&validator));

        // Test Finalize variant
        let finalize_task =
            ArgsTask::new(ArgsTaskType::Finalize(FinalizeTask {
                delegated_account: Pubkey::new_unique(),
            }));
        assert_serializable(&finalize_task.instruction(&validator));

        // Test Undelegate variant
        let undelegate_task: ArgsTask =
            ArgsTaskType::Undelegate(UndelegateTask {
                delegated_account: Pubkey::new_unique(),
                owner_program: Pubkey::new_unique(),
                rent_reimbursement: Pubkey::new_unique(),
            })
            .into();
        assert_serializable(&undelegate_task.instruction(&validator));

        // Test BaseAction variant
        let base_action: ArgsTask = ArgsTaskType::BaseAction(BaseActionTask {
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
        })
        .into();
        assert_serializable(&base_action.instruction(&validator));
    }

    // Test BufferTask variants
    #[test]
    fn test_buffer_task_instruction_serialization() {
        let validator = Pubkey::new_unique();

        let buffer_task = BufferTask::new_preparation_required(
            BufferTaskType::Commit(CommitTask {
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
            }),
        );
        assert_serializable(&buffer_task.instruction(&validator));
    }

    // Test preparation instructions
    #[test]
    fn test_preparation_instructions_serialization() {
        let authority = Pubkey::new_unique();

        // Test BufferTask preparation
        let buffer_task = BufferTask::new_preparation_required(
            BufferTaskType::Commit(CommitTask {
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
            }),
        );

        let PreparationState::Required(preparation_task) =
            buffer_task.preparation_state()
        else {
            panic!("invalid preparation state on creation!");
        };
        assert_serializable(&preparation_task.init_instruction(&authority));
        for ix in preparation_task.realloc_instructions(&authority) {
            assert_serializable(&ix);
        }
        for ix in preparation_task.write_instructions(&authority) {
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

#[test]
fn test_close_buffer_limit() {
    use solana_sdk::{
        compute_budget::ComputeBudgetInstruction, signature::Keypair,
        signer::Signer, transaction::Transaction,
    };

    use crate::transactions::{
        serialize_and_encode_base64, MAX_ENCODED_TRANSACTION_SIZE,
    };

    let task = CleanupTask {
        commit_id: 101,
        pubkey: Pubkey::new_unique(),
    };

    let compute_budget_ix =
        ComputeBudgetInstruction::set_compute_unit_limit(task.compute_units());
    let compute_unit_price_ix =
        ComputeBudgetInstruction::set_compute_unit_price(101);

    let authority = Keypair::new();
    let ixs_iter = (0..CleanupTask::max_tx_fit_count_with_budget())
        .into_iter()
        .map(|_| task.instruction(&authority.pubkey()));

    let mut ixs: Vec<_> = [compute_budget_ix, compute_unit_price_ix]
        .into_iter()
        .chain(ixs_iter)
        .collect();
    let tx = Transaction::new_with_payer(&ixs, Some(&authority.pubkey()));
    assert!(
        serialize_and_encode_base64(&tx).len() <= MAX_ENCODED_TRANSACTION_SIZE
    );

    // Check that 1 more ix will overflow allowed tx size
    ixs.push(task.instruction(&authority.pubkey()));
    let tx = Transaction::new_with_payer(&ixs, Some(&authority.pubkey()));
    assert!(
        serialize_and_encode_base64(&tx).len() > MAX_ENCODED_TRANSACTION_SIZE
    );
}
