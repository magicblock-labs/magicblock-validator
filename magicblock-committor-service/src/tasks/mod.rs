use compressed_delegation_client::types::{CommitArgs, FinalizeArgs};
use dlp::args::{
    CallHandlerArgs, CommitStateArgs, CommitStateFromBufferArgs, Context,
};
use dyn_clone::DynClone;
use log::debug;
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
    BaseAction, CommittedAccount,
};
use solana_pubkey::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};

use crate::{
    consts::MAX_WRITE_CHUNK_SIZE,
    tasks::{task_builder::CompressedData, visitor::Visitor},
};

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

#[derive(Clone, Debug)]
pub struct BufferPreparationInfo {
    pub commit_id: u64,
    pub pubkey: Pubkey,
    pub chunks_pda: Pubkey,
    pub buffer_pda: Pubkey,
    pub init_instruction: Instruction,
    pub realloc_instructions: Vec<Instruction>,
    pub write_instructions: Vec<Instruction>,
}

#[derive(Clone, Debug)]
pub enum TaskPreparationInfo {
    Buffer(BufferPreparationInfo),
    Compressed,
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

    /// Calls [`Visitor`] with specific task type
    fn visit(&self, visitor: &mut dyn Visitor);

    /// Returns true if task is compressed
    fn is_compressed(&self) -> bool;

    /// Sets compressed data for task
    fn set_compressed_data(&mut self, compressed_data: CompressedData);

    /// Gets compressed data for task
    fn get_compressed_data(&self) -> Option<CompressedData>;

    /// Delegated account for task
    fn delegated_account(&self) -> Option<Pubkey>;
}

dyn_clone::clone_trait_object!(BaseTask);

#[derive(Clone)]
pub struct CommitTask {
    pub commit_id: u64,
    pub allow_undelegation: bool,
    pub committed_account: CommittedAccount,
}

#[derive(Clone)]
pub struct CompressedCommitTask {
    pub commit_id: u64,
    pub allow_undelegation: bool,
    pub committed_account: CommittedAccount,
    pub compressed_data: CompressedData,
}

#[derive(Clone)]
pub struct UndelegateTask {
    pub delegated_account: Pubkey,
    pub owner_program: Pubkey,
    pub rent_reimbursement: Pubkey,
}

#[derive(Clone)]
pub struct CompressedUndelegateTask {
    pub delegated_account: Pubkey,
    pub owner_program: Pubkey,
    pub compressed_data: CompressedData,
}

#[derive(Clone)]
pub struct FinalizeTask {
    pub delegated_account: Pubkey,
}

#[derive(Clone)]
pub struct CompressedFinalizeTask {
    pub delegated_account: Pubkey,
    pub compressed_data: CompressedData,
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
    CompressedCommit(CompressedCommitTask),
    Finalize(FinalizeTask),
    CompressedFinalize(CompressedFinalizeTask),
    Undelegate(UndelegateTask), // Special action really
    CompressedUndelegate(CompressedUndelegateTask),
    BaseAction(BaseActionTask),
}

impl BaseTask for ArgsTask {
    fn instruction(&self, validator: &Pubkey) -> Instruction {
        match self {
            Self::Commit(value) => {
                debug!("CommitTask");
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
            Self::CompressedCommit(value) => {
                debug!("CompressedCommitTask");
                compressed_delegation_client::CommitBuilder::new()
                    .validator(*validator)
                    .delegated_account(value.committed_account.pubkey)
                    .args(CommitArgs {
                        current_compressed_delegated_account_data: value
                            .compressed_data
                            .compressed_delegation_record_bytes
                            .clone(),
                        new_data: value.committed_account.account.data.clone(),
                        account_meta: value.compressed_data.account_meta,
                        validity_proof: value.compressed_data.proof,
                        update_nonce: value.commit_id,
                        allow_undelegation: value.allow_undelegation,
                    })
                    .add_remaining_accounts(
                        &value.compressed_data.remaining_accounts,
                    )
                    .instruction()
            }
            Self::Finalize(value) => {
                debug!("FinalizeTask");
                dlp::instruction_builder::finalize(
                    *validator,
                    value.delegated_account,
                )
            }
            Self::CompressedFinalize(value) => {
                debug!("CompressedFinalizeTask");
                compressed_delegation_client::FinalizeBuilder::new()
                    .validator(*validator)
                    .delegated_account(value.delegated_account)
                    .args(FinalizeArgs {
                        current_compressed_delegated_account_data: value
                            .compressed_data
                            .compressed_delegation_record_bytes
                            .clone(),
                        account_meta: value.compressed_data.account_meta,
                        validity_proof: value.compressed_data.proof,
                    })
                    .add_remaining_accounts(
                        &value.compressed_data.remaining_accounts,
                    )
                    .instruction()
            }
            Self::CompressedUndelegate(value) => {
                debug!("CompressedUndelegateTask");
                compressed_delegation_client::UndelegateBuilder::new()
                    // .validator(*validator)
                    .delegated_account(value.delegated_account)
                    // .owner_program(value.owner_program)
                    // .rent_reimbursement(value.rent_reimbursement)
                    .add_remaining_accounts(
                        &value.compressed_data.remaining_accounts,
                    )
                    .instruction()
            }
            Self::Undelegate(value) => {
                debug!("UndelegateTask");
                dlp::instruction_builder::undelegate(
                    *validator,
                    value.delegated_account,
                    value.owner_program,
                    value.rent_reimbursement,
                )
            }
            Self::BaseAction(value) => {
                debug!("BaseActionTask");
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
            Self::BaseAction(_)
            | Self::Finalize(_)
            | Self::Undelegate(_)
            | Self::CompressedCommit(_)
            | Self::CompressedFinalize(_)
            | Self::CompressedUndelegate(_) => Err(self),
        }
    }

    fn preparation_info(&self, _: &Pubkey) -> Option<TaskPreparationInfo> {
        match self {
            Self::Commit(_)
            | Self::BaseAction(_)
            | Self::Finalize(_)
            | Self::Undelegate(_) => None,
            Self::CompressedCommit(_)
            | Self::CompressedFinalize(_)
            | Self::CompressedUndelegate(_) => {
                Some(TaskPreparationInfo::Compressed)
            }
        }
    }

    fn compute_units(&self) -> u32 {
        match self {
            Self::Commit(_) => 65_000,
            Self::CompressedCommit(_) => 150_000,
            Self::BaseAction(task) => task.action.compute_units,
            Self::Undelegate(_) => 50_000,
            Self::CompressedUndelegate(_) => 150_000,
            Self::Finalize(_) => 40_000,
            Self::CompressedFinalize(_) => 150_000,
        }
    }

    fn strategy(&self) -> TaskStrategy {
        TaskStrategy::Args
    }

    /// For tasks using Args strategy call corresponding `Visitor` method
    fn visit(&self, visitor: &mut dyn Visitor) {
        visitor.visit_args_task(self);
    }

    fn is_compressed(&self) -> bool {
        match self {
            Self::CompressedCommit(_)
            | Self::CompressedFinalize(_)
            | Self::CompressedUndelegate(_) => true,
            _ => false,
        }
    }

    fn set_compressed_data(&mut self, compressed_data: CompressedData) {
        match self {
            Self::CompressedCommit(value) => {
                value.compressed_data = compressed_data;
            }
            Self::CompressedFinalize(value) => {
                value.compressed_data = compressed_data;
            }
            Self::CompressedUndelegate(value) => {
                value.compressed_data = compressed_data;
            }
            _ => {}
        }
    }

    fn get_compressed_data(&self) -> Option<CompressedData> {
        match self {
            Self::CompressedCommit(value) => {
                Some(value.compressed_data.clone())
            }
            Self::CompressedFinalize(value) => {
                Some(value.compressed_data.clone())
            }
            Self::CompressedUndelegate(value) => {
                Some(value.compressed_data.clone())
            }
            _ => None,
        }
    }

    fn delegated_account(&self) -> Option<Pubkey> {
        match self {
            Self::Commit(value) => Some(value.committed_account.pubkey),
            Self::CompressedCommit(value) => {
                Some(value.committed_account.pubkey)
            }
            Self::Finalize(value) => Some(value.delegated_account),
            Self::CompressedFinalize(value) => Some(value.delegated_account),
            Self::Undelegate(value) => Some(value.delegated_account),
            Self::CompressedUndelegate(value) => Some(value.delegated_account),
            Self::BaseAction(_) => None,
        }
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

        let chunks_account_size = Chunks::struct_size(chunks.count()) as u64;
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

        Some(TaskPreparationInfo::Buffer(BufferPreparationInfo {
            commit_id: commit_task.commit_id,
            pubkey: commit_task.committed_account.pubkey,
            chunks_pda,
            buffer_pda: buffer_pda,
            init_instruction: init_instruction,
            realloc_instructions: realloc_instructions,
            write_instructions: write_instructions,
        }))
    }

    fn compute_units(&self) -> u32 {
        match self {
            Self::Commit(_) => 65_000,
        }
    }

    fn strategy(&self) -> TaskStrategy {
        TaskStrategy::Buffer
    }

    /// For tasks using Args strategy call corresponding `Visitor` method
    fn visit(&self, visitor: &mut dyn Visitor) {
        visitor.visit_buffer_task(self);
    }

    fn is_compressed(&self) -> bool {
        false
    }

    fn set_compressed_data(&mut self, _: CompressedData) {
        // No need to set compressed data for BufferTask
    }

    fn get_compressed_data(&self) -> Option<CompressedData> {
        None
    }

    fn delegated_account(&self) -> Option<Pubkey> {
        match self {
            Self::Commit(value) => Some(value.committed_account.pubkey),
        }
    }
}

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
        let commit_task = ArgsTask::Commit(CommitTask {
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

        let TaskPreparationInfo::Buffer(prep_info) =
            buffer_task.preparation_info(&authority).unwrap()
        else {
            panic!("Expected BufferTask preparation info");
        };
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
