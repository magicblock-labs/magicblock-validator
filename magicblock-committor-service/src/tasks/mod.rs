use dlp_api::{args::CallHandlerArgs, AccountSizeClass};
use magicblock_core::intent::{BaseAction, BaseActionCallback};
use magicblock_metrics::metrics::LabelValue;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

pub mod commit_delivery;
pub mod commit_finalize_task;
pub mod commit_stage_task;
pub mod intent_size_validator;
pub mod task_builder;
pub mod task_strategist;
pub mod utils;

pub use task_builder::TaskBuilderImpl;

use crate::tasks::commit_finalize_task::CommitFinalizeTask;

#[derive(Clone, Debug)]
pub enum BaseTaskImpl {
    CommitFinalize(CommitFinalizeTask),
    Undelegate(UndelegateTask),
    BaseAction(BaseActionTask),
}

impl BaseTask for BaseTaskImpl {
    fn instruction(&self, validator: &Pubkey) -> Instruction {
        match self {
            Self::CommitFinalize(value) => value.instruction(validator),
            Self::Undelegate(value) => value.instruction(validator),
            Self::BaseAction(value) => value.instruction(validator),
        }
    }

    fn try_optimize_tx_size(&mut self) -> bool {
        match self {
            Self::CommitFinalize(value) => value.try_optimize_tx_size(),
            _ => false,
        }
    }

    fn compute_units(&self) -> u32 {
        match self {
            Self::CommitFinalize(value) => value.compute_units(),
            Self::BaseAction(value) => value.compute_units(),
            Self::Undelegate(_) => 120_000,
        }
    }

    fn accounts_size_budget(&self) -> u32 {
        match self {
            Self::CommitFinalize(value) => value.accounts_size_budget(),
            Self::BaseAction(value) => value.accounts_size_budget(),
            Self::Undelegate(value) => {
                if value.include_undelegation_request {
                    dlp_api::instruction_builder::undelegate_with_request_size_budget(
                        AccountSizeClass::Huge,
                    )
                } else {
                    dlp_api::instruction_builder::undelegate_size_budget(
                        AccountSizeClass::Huge,
                    )
                }
            }
        }
    }
}

impl LabelValue for BaseTaskImpl {
    fn value(&self) -> &str {
        match self {
            Self::CommitFinalize(task) => {
                if task.is_buffer() {
                    "buffer_commit_finalize"
                } else {
                    "args_commit_finalize"
                }
            }
            Self::Undelegate(_) => "args_undelegate",
            Self::BaseAction(BaseActionTask::V1(_)) => "args_action",
            Self::BaseAction(BaseActionTask::V2(_)) => "args_action_v2",
        }
    }
}

/// A trait representing a task that can be executed on Base layer
pub trait BaseTask: Send + Sync + Clone {
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

    /// Attempts to optimize the task for smaller transaction size by switching
    /// to a buffer-based delivery. Returns `true` if optimization was applied.
    ///
    /// Deprecated: will be removed in the future. Optimization is a concern of
    /// the transaction strategist, not the task itself.
    fn try_optimize_tx_size(&mut self) -> bool;

    /// Returns [`Task`] budget
    fn compute_units(&self) -> u32;

    /// Returns the max accounts-data-size that can be used with SetLoadedAccountsDataSizeLimit
    fn accounts_size_budget(&self) -> u32;
}

#[derive(Clone, Debug)]
pub struct UndelegateTask {
    pub delegated_account: Pubkey,
    pub owner_program: Pubkey,
    pub rent_reimbursement: Pubkey,
    pub include_undelegation_request: bool,
}

impl UndelegateTask {
    pub fn instruction(&self, validator: &Pubkey) -> Instruction {
        if self.include_undelegation_request {
            dlp_api::instruction_builder::undelegate_with_request(
                *validator,
                self.delegated_account,
                self.owner_program,
                self.rent_reimbursement,
            )
        } else {
            dlp_api::instruction_builder::undelegate(
                *validator,
                self.delegated_account,
                self.owner_program,
                self.rent_reimbursement,
            )
        }
    }
}

impl From<UndelegateTask> for BaseTaskImpl {
    fn from(value: UndelegateTask) -> Self {
        Self::Undelegate(value)
    }
}

#[derive(Clone, Debug)]
pub enum BaseActionTask {
    V1(BaseActionTaskV1),
    V2(BaseActionTaskV2),
}

impl BaseActionTask {
    pub fn instruction(&self, validator: &Pubkey) -> Instruction {
        match self {
            Self::V1(value) => value.instruction(validator),
            Self::V2(value) => value.instruction(validator),
        }
    }

    pub fn action(&self) -> &BaseAction {
        match self {
            Self::V1(value) => &value.action,
            Self::V2(value) => &value.action,
        }
    }

    pub fn compute_units(&self) -> u32 {
        self.action().compute_units
    }

    pub fn extract_callback(&mut self) -> Option<BaseActionCallback> {
        match self {
            BaseActionTask::V1(value) => value.action.callback.take(),
            BaseActionTask::V2(value) => value.action.callback.take(),
        }
    }

    pub fn has_callback(&self) -> bool {
        match self {
            BaseActionTask::V1(value) => value.action.callback.is_some(),
            BaseActionTask::V2(value) => value.action.callback.is_some(),
        }
    }

    pub fn accounts_size_budget(&self) -> u32 {
        let action = self.action();
        // assume all other accounts are Small accounts.
        let other_accounts_budget = action.account_metas_per_program.len()
            as u32
            * AccountSizeClass::Small.size_budget();

        match self {
            Self::V1(_) => {
                dlp_api::instruction_builder::call_handler_size_budget(
                    AccountSizeClass::Medium,
                    other_accounts_budget,
                )
            }
            Self::V2(_) => {
                dlp_api::instruction_builder::call_handler_v2_size_budget(
                    AccountSizeClass::Medium,
                    AccountSizeClass::Medium,
                    other_accounts_budget,
                )
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct BaseActionTaskV1 {
    pub action: BaseAction,
}

impl BaseActionTaskV1 {
    pub fn instruction(&self, validator: &Pubkey) -> Instruction {
        let action = &self.action;
        #[allow(deprecated)]
        dlp_api::instruction_builder::call_handler(
            *validator,
            action.destination_program,
            action.escrow_authority,
            Self::account_metas_static(action),
            Self::call_handler_args_static(action),
        )
    }

    fn account_metas_static(action: &BaseAction) -> Vec<AccountMeta> {
        action
            .account_metas_per_program
            .iter()
            .map(|short_meta| AccountMeta {
                pubkey: short_meta.pubkey,
                is_writable: short_meta.is_writable,
                is_signer: false,
            })
            .collect()
    }

    fn call_handler_args_static(action: &BaseAction) -> CallHandlerArgs {
        CallHandlerArgs {
            data: action.data_per_program.data.clone(),
            escrow_index: action.data_per_program.escrow_index,
        }
    }
}

impl From<BaseActionTaskV1> for BaseActionTask {
    fn from(value: BaseActionTaskV1) -> Self {
        Self::V1(value)
    }
}

impl From<BaseActionTask> for BaseTaskImpl {
    fn from(value: BaseActionTask) -> Self {
        Self::BaseAction(value)
    }
}

#[derive(Clone, Debug)]
pub struct BaseActionTaskV2 {
    pub action: BaseAction,
    pub source_program: Pubkey,
}

impl BaseActionTaskV2 {
    pub fn instruction(&self, validator: &Pubkey) -> Instruction {
        let action = &self.action;
        dlp_api::instruction_builder::call_handler_v2(
            *validator,
            action.destination_program,
            self.source_program,
            action.escrow_authority,
            self.account_metas(),
            self.call_handler_args(),
        )
    }

    pub fn account_metas(&self) -> Vec<AccountMeta> {
        BaseActionTaskV1::account_metas_static(&self.action)
    }

    pub fn call_handler_args(&self) -> CallHandlerArgs {
        BaseActionTaskV1::call_handler_args_static(&self.action)
    }
}

impl From<BaseActionTaskV2> for BaseActionTask {
    fn from(value: BaseActionTaskV2) -> Self {
        Self::V2(value)
    }
}

#[cfg(test)]
mod tests {
    use dlp_api::{
        discriminator::DlpDiscriminator,
        pda::undelegation_request_pda_from_delegated_account,
    };
    use solana_pubkey::Pubkey;

    use super::UndelegateTask;

    #[test]
    fn test_undelegate_task_uses_request_account_when_included() {
        let delegated_account = Pubkey::new_unique();

        let ix = UndelegateTask {
            delegated_account,
            owner_program: Pubkey::new_unique(),
            rent_reimbursement: Pubkey::new_unique(),
            include_undelegation_request: true,
        }
        .instruction(&Pubkey::new_unique());

        assert_eq!(ix.program_id, dlp_api::id());
        assert_eq!(ix.data, DlpDiscriminator::Undelegate.to_vec());
        assert_eq!(ix.accounts.len(), 13);
        assert_eq!(
            ix.accounts[12].pubkey,
            undelegation_request_pda_from_delegated_account(&delegated_account)
        );
        assert!(ix.accounts[12].is_writable);
        assert!(!ix.accounts[12].is_signer);
    }
}

#[test]
fn test_close_buffer_limit() {
    use solana_compute_budget_interface::ComputeBudgetInstruction;
    use solana_keypair::Keypair;
    use solana_signer::Signer;
    use solana_transaction::Transaction;
    use tracing::info;

    use crate::{
        tasks::{commit_stage_task::CleanupTask, utils::TransactionUtils},
        test_utils,
        transactions::{
            serialized_transaction_size, MAX_TRANSACTION_WIRE_SIZE,
        },
    };

    test_utils::init_test_logger();

    let authority = Keypair::new();

    // Budget ixs (fixed)
    let compute_budget_ix =
        ComputeBudgetInstruction::set_compute_unit_limit(30_000);
    let compute_unit_price_ix =
        ComputeBudgetInstruction::set_compute_unit_price(101);

    // Each task unique: commit_id increments; pubkey is new_unique each time
    let base_commit_id = 101u64;
    let ixs_iter = (0..CleanupTask::max_tx_fit_count_with_budget()).map(|i| {
        let task = CleanupTask {
            commit_id: base_commit_id + i as u64,
            pubkey: Pubkey::new_unique(),
        };
        task.instruction(&authority.pubkey())
    });

    let mut ixs: Vec<_> = [compute_budget_ix, compute_unit_price_ix]
        .into_iter()
        .chain(ixs_iter)
        .collect();
    ixs.push(TransactionUtils::uniqueness_noop_instruction(42));

    let tx = Transaction::new_with_payer(&ixs, Some(&authority.pubkey()));
    let tx_size = serialized_transaction_size(&tx);
    info!(transaction_size = tx_size, "Cleanup task transaction size");
    assert!(tx_size <= MAX_TRANSACTION_WIRE_SIZE);

    // One more unique task should overflow
    let overflow_task = CleanupTask {
        commit_id: base_commit_id
            + CleanupTask::max_tx_fit_count_with_budget() as u64,
        pubkey: Pubkey::new_unique(),
    };
    let uniqueness_noop = ixs.pop().expect("uniqueness noop");
    ixs.push(overflow_task.instruction(&authority.pubkey()));
    ixs.push(uniqueness_noop);

    let tx = Transaction::new_with_payer(&ixs, Some(&authority.pubkey()));
    assert!(serialized_transaction_size(&tx) > MAX_TRANSACTION_WIRE_SIZE);
}
