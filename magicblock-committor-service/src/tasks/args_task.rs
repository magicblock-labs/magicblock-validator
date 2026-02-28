use dlp::{
    args::{CommitDiffArgs, CommitStateArgs},
    compute_diff,
    AccountSizeClass,
};
use dlp_api::instruction_builder::{
    call_handler_size_budget, call_handler_v2_size_budget,
    commit_diff_size_budget, commit_size_budget, finalize_size_budget,
    undelegate_size_budget,
};
use magicblock_metrics::metrics::LabelValue;
use solana_account::ReadableAccount;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

#[cfg(test)]
use crate::tasks::TaskStrategy;
use crate::tasks::{
    buffer_task::{BufferTask, BufferTaskType},
    visitor::Visitor,
    BaseActionTask, BaseActionV2Task, BaseTask, BaseTaskError, BaseTaskResult,
    CommitDiffTask, CommitTask, FinalizeTask, PreparationState, TaskType,
    UndelegateTask,
};

/// Task that will be executed on Base layer via arguments
#[derive(Clone)]
pub enum ArgsTaskType {
    Commit(CommitTask),
    CommitDiff(CommitDiffTask),
    Finalize(FinalizeTask),
    Undelegate(UndelegateTask), // Special action really
    BaseAction(BaseActionTask),
    BaseActionV2(BaseActionV2Task),
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
    fn program_id(&self) -> Pubkey {
        dlp::id()
    }

    fn instruction(&self, validator: &Pubkey) -> Instruction {
        match &self.task_type {
            ArgsTaskType::Commit(value) => {
                let args = CommitStateArgs {
                    nonce: value.commit_id,
                    lamports: value.committed_account.account.lamports,
                    data: value.committed_account.account.data.clone(),
                    allow_undelegation: value.allow_undelegation,
                };
                dlp_api::instruction_builder::commit_state(
                    *validator,
                    value.committed_account.pubkey,
                    value.committed_account.account.owner,
                    args,
                )
            }
            ArgsTaskType::CommitDiff(value) => {
                let args = CommitDiffArgs {
                    nonce: value.commit_id,
                    lamports: value.committed_account.account.lamports,
                    diff: compute_diff(
                        value.base_account.data(),
                        value.committed_account.account.data(),
                    )
                    .to_vec(),
                    allow_undelegation: value.allow_undelegation,
                };

                dlp_api::instruction_builder::commit_diff(
                    *validator,
                    value.committed_account.pubkey,
                    value.committed_account.account.owner,
                    args,
                )
            }
            ArgsTaskType::Finalize(value) => {
                dlp_api::instruction_builder::finalize(
                    *validator,
                    value.delegated_account,
                )
            }
            ArgsTaskType::Undelegate(value) => {
                dlp_api::instruction_builder::undelegate(
                    *validator,
                    value.delegated_account,
                    value.owner_program,
                    value.rent_reimbursement,
                )
            }
            ArgsTaskType::BaseAction(value) => {
                let action = &value.action;
                dlp_api::instruction_builder::call_handler(
                    *validator,
                    action.destination_program,
                    action.escrow_authority,
                    value.account_metas(),
                    value.call_handler_args(),
                )
            }
            ArgsTaskType::BaseActionV2(value) => {
                let action = &value.action;
                dlp_api::instruction_builder::call_handler_v2(
                    *validator,
                    action.destination_program,
                    value.source_program,
                    action.escrow_authority,
                    value.account_metas(),
                    value.call_handler_args(),
                )
            }
        }
    }

    fn try_optimize_tx_size(
        self: Box<Self>,
    ) -> Result<Box<dyn BaseTask>, Box<dyn BaseTask>> {
        match self.task_type {
            ArgsTaskType::Commit(value) => {
                Ok(Box::new(BufferTask::new_preparation_required(
                    BufferTaskType::Commit(value),
                )))
            }
            ArgsTaskType::CommitDiff(value) => {
                Ok(Box::new(BufferTask::new_preparation_required(
                    BufferTaskType::CommitDiff(value),
                )))
            }
            ArgsTaskType::BaseAction(_)
            | ArgsTaskType::BaseActionV2(_)
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
            ArgsTaskType::Commit(_) => 70_000,
            ArgsTaskType::CommitDiff(_) => 70_000,
            ArgsTaskType::BaseAction(task) => task.action.compute_units,
            ArgsTaskType::BaseActionV2(task) => task.action.compute_units,
            ArgsTaskType::Undelegate(_) => 70_000,
            ArgsTaskType::Finalize(_) => 70_000,
        }
    }

    fn accounts_size_budget(&self) -> u32 {
        match &self.task_type {
            ArgsTaskType::Commit(task) => {
                commit_size_budget(AccountSizeClass::Dynamic(
                    task.committed_account.account.data.len() as u32,
                ))
            }
            ArgsTaskType::CommitDiff(task) => {
                commit_diff_size_budget(AccountSizeClass::Dynamic(
                    task.committed_account.account.data.len() as u32,
                ))
            }
            ArgsTaskType::BaseAction(task) => {
                // assume all other accounts are Small accounts.
                let other_accounts_budget =
                    task.action.account_metas_per_program.len() as u32
                        * AccountSizeClass::Small.size_budget();

                call_handler_size_budget(
                    AccountSizeClass::Medium,
                    other_accounts_budget,
                )
            }
            ArgsTaskType::BaseActionV2(task) => {
                // assume all other accounts are Small accounts.
                let other_accounts_budget =
                    task.action.account_metas_per_program.len() as u32
                        * AccountSizeClass::Small.size_budget();

                call_handler_v2_size_budget(
                    AccountSizeClass::Medium,
                    AccountSizeClass::Medium,
                    other_accounts_budget,
                )
            }
            ArgsTaskType::Undelegate(_) => {
                undelegate_size_budget(AccountSizeClass::Huge)
            }
            ArgsTaskType::Finalize(_) => {
                finalize_size_budget(AccountSizeClass::Huge)
            }
        }
    }

    #[cfg(test)]
    fn strategy(&self) -> TaskStrategy {
        TaskStrategy::Args
    }

    fn task_type(&self) -> TaskType {
        match &self.task_type {
            ArgsTaskType::Commit(_) => TaskType::Commit,
            ArgsTaskType::CommitDiff(_) => TaskType::Commit,
            ArgsTaskType::BaseAction(_) | ArgsTaskType::BaseActionV2(_) => {
                TaskType::Action
            }
            ArgsTaskType::Undelegate(_) => TaskType::Undelegate,
            ArgsTaskType::Finalize(_) => TaskType::Finalize,
        }
    }

    /// For tasks using Args strategy call corresponding `Visitor` method
    fn visit(&self, visitor: &mut dyn Visitor) {
        visitor.visit_args_task(self);
    }

    fn reset_commit_id(&mut self, commit_id: u64) {
        match &mut self.task_type {
            ArgsTaskType::Commit(task) => {
                task.commit_id = commit_id;
            }
            ArgsTaskType::CommitDiff(task) => {
                task.commit_id = commit_id;
            }
            ArgsTaskType::BaseAction(_)
            | ArgsTaskType::BaseActionV2(_)
            | ArgsTaskType::Finalize(_)
            | ArgsTaskType::Undelegate(_) => {}
        };
    }
}

impl LabelValue for ArgsTask {
    fn value(&self) -> &str {
        match self.task_type {
            ArgsTaskType::Commit(_) => "args_commit",
            ArgsTaskType::CommitDiff(_) => "args_commit_diff",
            ArgsTaskType::BaseAction(_) => "args_action",
            ArgsTaskType::BaseActionV2(_) => "args_action_v2",
            ArgsTaskType::Finalize(_) => "args_finalize",
            ArgsTaskType::Undelegate(_) => "args_undelegate",
        }
    }
}
