use compressed_delegation_client::types::{CommitArgs, FinalizeArgs};
use dlp::args::{CallHandlerArgs, CommitStateArgs};
use solana_pubkey::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};

#[cfg(test)]
use crate::tasks::TaskStrategy;
use crate::tasks::{
    buffer_task::{BufferTask, BufferTaskType},
    task_builder::CompressedData,
    visitor::Visitor,
    BaseActionTask, BaseTask, BaseTaskError, BaseTaskResult, CommitTask,
    CompressedCommitTask, CompressedFinalizeTask, CompressedUndelegateTask,
    FinalizeTask, PreparationState, PreparationTask, TaskType, UndelegateTask,
};

/// Task that will be executed on Base layer via arguments
#[derive(Clone)]
pub enum ArgsTaskType {
    Commit(CommitTask),
    CompressedCommit(CompressedCommitTask),
    Finalize(FinalizeTask),
    CompressedFinalize(CompressedFinalizeTask),
    Undelegate(UndelegateTask), // Special action really
    CompressedUndelegate(CompressedUndelegateTask),
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
            ArgsTaskType::CompressedCommit(value) => {
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
            ArgsTaskType::Finalize(value) => {
                dlp::instruction_builder::finalize(
                    *validator,
                    value.delegated_account,
                )
            }
            ArgsTaskType::CompressedFinalize(value) => {
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
            ArgsTaskType::CompressedUndelegate(value) => {
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
            | ArgsTaskType::Undelegate(_)
            | ArgsTaskType::CompressedCommit(_)
            | ArgsTaskType::CompressedFinalize(_)
            | ArgsTaskType::CompressedUndelegate(_) => Err(self),
        }
    }

    /// Only prepare compressed tasks [`ArgsTaskType`] type
    fn preparation_state(&self) -> &PreparationState {
        match &self.task_type {
            ArgsTaskType::Commit(_)
            | ArgsTaskType::BaseAction(_)
            | ArgsTaskType::Finalize(_)
            | ArgsTaskType::Undelegate(_) => &self.preparation_state,
            ArgsTaskType::CompressedCommit(_)
            | ArgsTaskType::CompressedFinalize(_)
            | ArgsTaskType::CompressedUndelegate(_) => {
                &PreparationState::Required(PreparationTask::Compressed)
            }
        }
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
            ArgsTaskType::CompressedCommit(_) => 250_000,
            ArgsTaskType::BaseAction(task) => task.action.compute_units,
            ArgsTaskType::Undelegate(_) => 70_000,
            ArgsTaskType::CompressedUndelegate(_) => 250_000,
            ArgsTaskType::Finalize(_) => 70_000,
            ArgsTaskType::CompressedFinalize(_) => 250_000,
        }
    }

    #[cfg(test)]
    fn strategy(&self) -> TaskStrategy {
        TaskStrategy::Args
    }

    fn task_type(&self) -> TaskType {
        match &self.task_type {
            ArgsTaskType::Commit(_) => TaskType::Commit,
            ArgsTaskType::CompressedCommit(_) => TaskType::CompressedCommit,
            ArgsTaskType::BaseAction(_) => TaskType::Action,
            ArgsTaskType::Undelegate(_) => TaskType::Undelegate,
            ArgsTaskType::CompressedUndelegate(_) => {
                TaskType::CompressedUndelegate
            }
            ArgsTaskType::Finalize(_) => TaskType::Finalize,
            ArgsTaskType::CompressedFinalize(_) => TaskType::CompressedFinalize,
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

    fn is_compressed(&self) -> bool {
        matches!(
            &self.task_type,
            ArgsTaskType::CompressedCommit(_)
                | ArgsTaskType::CompressedFinalize(_)
                | ArgsTaskType::CompressedUndelegate(_)
        )
    }

    fn set_compressed_data(&mut self, compressed_data: CompressedData) {
        match &mut self.task_type {
            ArgsTaskType::CompressedCommit(value) => {
                value.compressed_data = compressed_data;
            }
            ArgsTaskType::CompressedFinalize(value) => {
                value.compressed_data = compressed_data;
            }
            ArgsTaskType::CompressedUndelegate(value) => {
                value.compressed_data = compressed_data;
            }
            _ => {}
        }
    }

    fn get_compressed_data(&self) -> Option<CompressedData> {
        match &self.task_type {
            ArgsTaskType::CompressedCommit(value) => {
                Some(value.compressed_data.clone())
            }
            ArgsTaskType::CompressedFinalize(value) => {
                Some(value.compressed_data.clone())
            }
            ArgsTaskType::CompressedUndelegate(value) => {
                Some(value.compressed_data.clone())
            }
            _ => None,
        }
    }

    fn delegated_account(&self) -> Option<Pubkey> {
        match &self.task_type {
            ArgsTaskType::Commit(value) => Some(value.committed_account.pubkey),
            ArgsTaskType::CompressedCommit(value) => {
                Some(value.committed_account.pubkey)
            }
            ArgsTaskType::Finalize(value) => Some(value.delegated_account),
            ArgsTaskType::CompressedFinalize(value) => {
                Some(value.delegated_account)
            }
            ArgsTaskType::Undelegate(value) => Some(value.delegated_account),
            ArgsTaskType::CompressedUndelegate(value) => {
                Some(value.delegated_account)
            }
            ArgsTaskType::BaseAction(_) => None,
        }
    }
}
