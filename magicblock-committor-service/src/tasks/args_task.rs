use dlp::args::{CallHandlerArgs, CommitStateArgs};
use solana_pubkey::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};

#[cfg(test)]
use crate::tasks::TaskStrategy;
use crate::tasks::{
    buffer_task::{BufferTask, BufferTaskType},
    visitor::Visitor,
    BaseActionTask, BaseTask, BaseTaskError, BaseTaskResult, CommitTask,
    FinalizeTask, PreparationState, TaskType, UndelegateTask,
};

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

impl From<ArgsTaskType> for ArgsTask {
    fn from(value: ArgsTaskType) -> Self {
        Self::new(value)
    }
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
        match self.task_type {
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
            // Do nothing
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

    #[cfg(test)]
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

        commit_task.commit_id = commit_id;
    }
}
