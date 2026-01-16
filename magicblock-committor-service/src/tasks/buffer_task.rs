use dlp::{
    args::CommitStateFromBufferArgs,
    compute_diff,
    instruction_builder::{commit_diff_size_budget, commit_size_budget},
    AccountSizeClass,
};
use magicblock_committor_program::Chunks;
use magicblock_metrics::metrics::LabelValue;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

#[cfg(any(test, feature = "dev-context-only-utils"))]
use super::args_task::ArgsTaskType;
#[cfg(test)]
use crate::tasks::TaskStrategy;
use crate::{
    consts::MAX_WRITE_CHUNK_SIZE,
    tasks::{
        visitor::Visitor, BaseTask, BaseTaskError, BaseTaskResult,
        BufferPreparationTask, CommitDiffTask, CommitTask, PreparationState,
        PreparationTask, TaskType,
    },
};

/// Tasks that could be executed using buffers
#[derive(Clone)]
pub enum BufferTaskType {
    Commit(CommitTask),
    CommitDiff(CommitDiffTask),
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
        match task_type {
            BufferTaskType::Commit(task) => {
                let data = task.committed_account.account.data.clone();
                let chunks =
                    Chunks::from_data_length(data.len(), MAX_WRITE_CHUNK_SIZE);

                PreparationState::Required(PreparationTask::Buffer(
                    BufferPreparationTask {
                        commit_id: task.commit_id,
                        pubkey: task.committed_account.pubkey,
                        committed_data: data,
                        chunks,
                    },
                ))
            }

            BufferTaskType::CommitDiff(task) => {
                let diff = compute_diff(
                    &task.base_account.data,
                    &task.committed_account.account.data,
                )
                .to_vec();
                let chunks =
                    Chunks::from_data_length(diff.len(), MAX_WRITE_CHUNK_SIZE);

                PreparationState::Required(PreparationTask::Buffer(
                    BufferPreparationTask {
                        commit_id: task.commit_id,
                        pubkey: task.committed_account.pubkey,
                        committed_data: diff,
                        chunks,
                    },
                ))
            }
        }
    }
}

#[cfg(any(test, feature = "dev-context-only-utils"))]
impl From<ArgsTaskType> for BufferTaskType {
    fn from(value: ArgsTaskType) -> Self {
        match value {
            ArgsTaskType::Commit(task) => BufferTaskType::Commit(task),
            ArgsTaskType::CommitDiff(task) => BufferTaskType::CommitDiff(task),
            _ => unimplemented!(
                "Only commit task can be BufferTask currently. Fix your tests"
            ),
        }
    }
}

impl BaseTask for BufferTask {
    fn instruction(&self, validator: &Pubkey) -> Instruction {
        match &self.task_type {
            BufferTaskType::Commit(task) => {
                let commit_id_slice = task.commit_id.to_le_bytes();
                let (commit_buffer_pubkey, _) =
                    magicblock_committor_program::pdas::buffer_pda(
                        validator,
                        &task.committed_account.pubkey,
                        &commit_id_slice,
                    );

                dlp::instruction_builder::commit_state_from_buffer(
                    *validator,
                    task.committed_account.pubkey,
                    task.committed_account.account.owner,
                    commit_buffer_pubkey,
                    CommitStateFromBufferArgs {
                        nonce: task.commit_id,
                        lamports: task.committed_account.account.lamports,
                        allow_undelegation: task.allow_undelegation,
                    },
                )
            }
            BufferTaskType::CommitDiff(task) => {
                let commit_id_slice = task.commit_id.to_le_bytes();
                let (commit_buffer_pubkey, _) =
                    magicblock_committor_program::pdas::buffer_pda(
                        validator,
                        &task.committed_account.pubkey,
                        &commit_id_slice,
                    );

                dlp::instruction_builder::commit_diff_from_buffer(
                    *validator,
                    task.committed_account.pubkey,
                    task.committed_account.account.owner,
                    commit_buffer_pubkey,
                    CommitStateFromBufferArgs {
                        nonce: task.commit_id,
                        lamports: task.committed_account.account.lamports,
                        allow_undelegation: task.allow_undelegation,
                    },
                )
            }
        }
    }

    /// No further optimizations
    fn try_optimize_tx_size(
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
            self.preparation_state = new_state;
            Ok(())
        }
    }

    fn compute_units(&self) -> u32 {
        match self.task_type {
            BufferTaskType::Commit(_) => 70_000,
            BufferTaskType::CommitDiff(_) => 70_000,
        }
    }

    fn accounts_size_budget(&self) -> u32 {
        match self.task_type {
            BufferTaskType::Commit(_) => {
                commit_size_budget(AccountSizeClass::Huge)
            }
            BufferTaskType::CommitDiff(_) => {
                commit_diff_size_budget(AccountSizeClass::Huge)
            }
        }
    }

    #[cfg(test)]
    fn strategy(&self) -> TaskStrategy {
        TaskStrategy::Buffer
    }

    fn task_type(&self) -> TaskType {
        match self.task_type {
            BufferTaskType::Commit(_) => TaskType::Commit,
            BufferTaskType::CommitDiff(_) => TaskType::Commit,
        }
    }

    /// For tasks using Args strategy call corresponding `Visitor` method
    fn visit(&self, visitor: &mut dyn Visitor) {
        visitor.visit_buffer_task(self);
    }

    fn reset_commit_id(&mut self, commit_id: u64) {
        match &mut self.task_type {
            BufferTaskType::Commit(task) => {
                task.commit_id = commit_id;
            }
            BufferTaskType::CommitDiff(task) => {
                task.commit_id = commit_id;
            }
        };

        self.preparation_state = Self::preparation_required(&self.task_type)
    }

    fn is_compressed(&self) -> bool {
        false
    }

    fn set_compressed_data(
        &mut self,
        _compressed_data: super::task_builder::CompressedData,
    ) {
        // No-op
    }

    fn get_compressed_data(
        &self,
    ) -> Option<&super::task_builder::CompressedData> {
        None
    }

    fn delegated_account(&self) -> Option<Pubkey> {
        match &self.task_type {
            BufferTaskType::Commit(value) => {
                Some(value.committed_account.pubkey)
            }
            BufferTaskType::CommitDiff(value) => {
                Some(value.committed_account.pubkey)
            }
        }
    }
}

impl LabelValue for BufferTask {
    fn value(&self) -> &str {
        match self.task_type {
            BufferTaskType::Commit(_) => "buffer_commit",
            BufferTaskType::CommitDiff(_) => "buffer_commit_diff",
        }
    }
}
