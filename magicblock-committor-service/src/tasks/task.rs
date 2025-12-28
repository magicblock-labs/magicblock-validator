use magicblock_metrics::metrics::LabelValue;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

use super::{BufferLifecycle, TaskInstruction, TaskResult};
use crate::tasks::{
    visitor::Visitor, BaseActionTask, CommitTask, DeliveryStrategy,
    FinalizeTask, PreparationState, TaskError, TaskType, UndelegateTask,
};

/// Task to be executed on the Base layer.  
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Task {
    Commit(CommitTask),
    Finalize(FinalizeTask),
    Undelegate(UndelegateTask), // Special action really
    BaseAction(BaseActionTask),
}

impl TaskInstruction for Task {
    fn instruction(&self, validator: &Pubkey) -> Instruction {
        match &self {
            Task::Commit(value) => value.instruction(validator),
            Task::Finalize(value) => value.instruction(validator),
            Task::Undelegate(value) => value.instruction(validator),
            Task::BaseAction(value) => value.instruction(validator),
        }
    }
}
impl Task {
    pub fn involved_accounts(&self, validator: &Pubkey) -> Vec<Pubkey> {
        // TODO (snawaz): rewrite it.
        // currently it is slow as it discards heavy computations and memory allocations.
        self.instruction(validator)
            .accounts
            .iter()
            .map(|meta| meta.pubkey)
            .collect()
    }

    #[allow(clippy::result_large_err)]
    pub fn try_optimize_tx_size(self) -> Result<Task, Task> {
        // TODO (snawaz): do two things:
        // 1. this does not properly handle preparation state as both ArgsTask
        // and CommitTask have this. Only CommitTask needs to have this.
        // 3. Remove PreparationState.
        // 4. Instead have enum LifecycleTask { PreparationTask, CleanupTask } or struct (ee
        //    [2].
        // 5. NotNeeded is not needed (pun not intended). Instead use Option<LifecycleTask>
        //
        // ref:
        // 1: https://chatgpt.com/s/t_691e1c39f47081919efcc73a2f599cf9
        // 2: https://chatgpt.com/s/t_691e1d7e82a08191963b43c6c8ad7a96
        match self {
            Task::Commit(value) => value
                .try_optimize_tx_size()
                .map(Task::Commit)
                .map_err(Task::Commit),
            Task::BaseAction(_) | Task::Finalize(_) | Task::Undelegate(_) => {
                Err(self)
            }
        }
    }

    /// Nothing to prepare for [`ArgsTaskType`] type
    pub fn preparation_state(&self) -> &PreparationState {
        todo!()
    }

    pub fn switch_preparation_state(
        &mut self,
        new_state: PreparationState,
    ) -> TaskResult<()> {
        if !matches!(new_state, PreparationState::NotNeeded) {
            Err(TaskError::PreparationStateTransitionError)
        } else {
            // Do nothing
            Ok(())
        }
    }

    pub fn lifecycle(&self) -> Option<&BufferLifecycle> {
        match &self {
            Task::Commit(commit) => commit.lifecycle(),
            Task::BaseAction(_) => None,
            Task::Undelegate(_) => None,
            Task::Finalize(_) => None,
        }
    }

    pub fn compute_units(&self) -> u32 {
        match &self {
            Task::Commit(_) => 70_000,
            Task::BaseAction(task) => task.action.compute_units,
            Task::Undelegate(_) => 70_000,
            Task::Finalize(_) => 70_000,
        }
    }

    pub fn strategy(&self) -> DeliveryStrategy {
        match &self {
            Task::Commit(commit) => commit.task_strategy(),
            Task::BaseAction(_) => DeliveryStrategy::Args,
            Task::Undelegate(_) => DeliveryStrategy::Args,
            Task::Finalize(_) => DeliveryStrategy::Args,
        }
    }

    pub fn task_type(&self) -> TaskType {
        match &self {
            Task::Commit(_) => TaskType::Commit,
            Task::BaseAction(_) => TaskType::Action,
            Task::Undelegate(_) => TaskType::Undelegate,
            Task::Finalize(_) => TaskType::Finalize,
        }
    }

    /// For tasks using Args strategy call corresponding `Visitor` method
    pub fn visit(&self, visitor: &mut dyn Visitor) {
        visitor.visit_task(self);
    }

    pub fn reset_commit_id(&mut self, commit_id: u64) {
        let Task::Commit(commit_task) = self else {
            return;
        };
        commit_task.reset_commit_id(commit_id);
    }
}

impl LabelValue for Task {
    fn value(&self) -> &str {
        use Task::*;

        use super::DataDeliveryStrategy::*;
        match self {
            Commit(commit) => match commit.delivery {
                StateInArgs => "state_args_commit",
                StateInBuffer { .. } => "state_buffer_commit",
                DiffInArgs { .. } => "diff_args_commit",
                DiffInBuffer { .. } => "diff_buffer_commit",
            },
            Finalize(_) => "finalize",
            Undelegate(_) => "undelegate",
            BaseAction(_) => "base_action",
        }
    }
}
