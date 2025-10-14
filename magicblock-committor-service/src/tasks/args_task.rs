use dlp::{
    args::{CallHandlerArgs, CommitDiffArgs, CommitStateArgs},
    compute_diff,
};
use magicblock_metrics::metrics::LabelValue;
use solana_account::ReadableAccount;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    instruction::{AccountMeta, Instruction},
};

#[cfg(test)]
use crate::tasks::TaskStrategy;
use crate::{
    config::ChainConfig,
    tasks::{
        buffer_task::{BufferTask, BufferTaskType},
        visitor::Visitor,
        BaseActionTask, BaseTask, BaseTaskError, BaseTaskResult, CommitTask,
        FinalizeTask, PreparationState, TaskType, UndelegateTask,
    },
    ComputeBudgetConfig,
};

/// Task that will be executed on Base layer via arguments
#[derive(Clone)]
pub enum ArgsTaskType {
    Commit(CommitTask),
    CommitDiff(CommitTask),
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
            ArgsTaskType::CommitDiff(value) => {
                let chain_config =
                    ChainConfig::local(ComputeBudgetConfig::new(1_000_000));

                let rpc_client = RpcClient::new_with_commitment(
                    chain_config.rpc_uri.to_string(),
                    CommitmentConfig {
                        commitment: chain_config.commitment,
                    },
                );

                let account = match rpc_client
                    .get_account(&value.committed_account.pubkey)
                {
                    Ok(account) => account,
                    Err(e) => {
                        log::warn!("Fallback to commit_state and send full-bytes, as rpc failed to fetch the delegated-account from base chain, commmit_id: {} , error: {}", value.commit_id, e);
                        let args = CommitStateArgs {
                            nonce: value.commit_id,
                            lamports: value.committed_account.account.lamports,
                            data: value.committed_account.account.data.clone(),
                            allow_undelegation: value.allow_undelegation,
                        };
                        return dlp::instruction_builder::commit_state(
                            *validator,
                            value.committed_account.pubkey,
                            value.committed_account.account.owner,
                            args,
                        );
                    }
                };

                let args = CommitDiffArgs {
                    nonce: value.commit_id,
                    lamports: value.committed_account.account.lamports,
                    diff: compute_diff(
                        account.data(),
                        value.committed_account.account.data(),
                    )
                    .to_vec(),
                    allow_undelegation: value.allow_undelegation,
                };
                dlp::instruction_builder::commit_diff(
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
            // TODO (snawaz): discuss this with reviewers
            ArgsTaskType::CommitDiff(_) => Err(self),
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
            ArgsTaskType::Commit(_) => 70_000,
            ArgsTaskType::CommitDiff(_) => 65_000,
            ArgsTaskType::BaseAction(task) => task.action.compute_units,
            ArgsTaskType::Undelegate(_) => 70_000,
            ArgsTaskType::Finalize(_) => 70_000,
        }
    }

    #[cfg(test)]
    fn strategy(&self) -> TaskStrategy {
        TaskStrategy::Args
    }

    fn task_type(&self) -> TaskType {
        match &self.task_type {
            ArgsTaskType::Commit(_) => TaskType::Commit,
            // TODO (snawaz): What should we use here? Commit (in the sense of "category of task"), or add a
            // new variant "CommitDiff" to indicate a specific instruction?
            ArgsTaskType::CommitDiff(_) => TaskType::Commit,
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
        // TODO (snawaz): handle CommitDiff as well? what is it about?
        let ArgsTaskType::Commit(commit_task) = &mut self.task_type else {
            return;
        };

        commit_task.commit_id = commit_id;
    }
}

impl LabelValue for ArgsTask {
    fn value(&self) -> &str {
        match self.task_type {
            ArgsTaskType::Commit(_) => "args_commit",
            ArgsTaskType::BaseAction(_) => "args_action",
            ArgsTaskType::Finalize(_) => "args_finalize",
            ArgsTaskType::Undelegate(_) => "args_undelegate",
        }
    }
}
