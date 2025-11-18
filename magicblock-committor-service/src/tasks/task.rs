use dlp::args::CallHandlerArgs;
use solana_pubkey::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};

use crate::tasks::TaskStrategy;
use crate::tasks::{
    visitor::Visitor, BaseActionTask, CommitTask, FinalizeTask,
    PreparationState, TaskError, TaskResult, TaskType, UndelegateTask,
};

use super::BufferLifecycle;

/// Task that will be executed on Base layer via arguments
#[derive(Debug, Clone)]
pub enum Task {
    Commit(CommitTask),
    Finalize(FinalizeTask),
    Undelegate(UndelegateTask), // Special action really
    BaseAction(BaseActionTask),
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

    pub fn instruction(&self, validator: &Pubkey) -> Instruction {
        match &self {
            Task::Commit(value) => value.create_commit_ix(validator),
            Task::Finalize(value) => dlp::instruction_builder::finalize(
                *validator,
                value.delegated_account,
            ),
            Task::Undelegate(value) => dlp::instruction_builder::undelegate(
                *validator,
                value.delegated_account,
                value.owner_program,
                value.rent_reimbursement,
            ),
            Task::BaseAction(value) => {
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
                        data: action.data_per_program.data.clone(),
                        escrow_index: action.data_per_program.escrow_index,
                    },
                )
            }
        }
    }

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

    pub fn compute_units(&self) -> u32 {
        match &self {
            Task::Commit(_) => 70_000,
            Task::BaseAction(task) => task.action.compute_units,
            Task::Undelegate(_) => 70_000,
            Task::Finalize(_) => 70_000,
        }
    }

    pub fn strategy(&self) -> TaskStrategy {
        match &self {
            Task::Commit(commit) => commit.task_strategy(),
            Task::BaseAction(_) => TaskStrategy::Args,
            Task::Undelegate(_) => TaskStrategy::Args,
            Task::Finalize(_) => TaskStrategy::Args,
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
