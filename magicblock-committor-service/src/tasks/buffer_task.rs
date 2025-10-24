use dlp::args::CommitStateFromBufferArgs;
use magicblock_committor_program::Chunks;
use solana_pubkey::Pubkey;
use solana_sdk::instruction::Instruction;

#[cfg(test)]
use crate::tasks::TaskStrategy;
use crate::{
    consts::MAX_WRITE_CHUNK_SIZE,
    tasks::{
        visitor::Visitor, BaseTask, BaseTaskError, BaseTaskResult, CommitTask,
        PreparationState, PreparationTask, TaskType,
    },
};

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

        PreparationState::Required(PreparationTask {
            commit_id: commit_task.commit_id,
            pubkey: commit_task.committed_account.pubkey,
            committed_data,
            chunks,
        })
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
            self.preparation_state = new_state;
            Ok(())
        }
    }

    fn compute_units(&self) -> u32 {
        match self.task_type {
            BufferTaskType::Commit(_) => 65_000,
        }
    }

    #[cfg(test)]
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
