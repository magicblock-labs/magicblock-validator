use ::metrics::metrics::LabelValue;
use delegation_program_api::{AccountSizeClass, args::CallHandlerArgs};
use mbv_core::intent::{BaseAction, BaseActionCallback};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

pub(crate) mod buffer_task;
pub(crate) mod commit_delivery;
pub(crate) mod commit_finalize_task;
pub(crate) mod task_strategist;
pub(crate) mod utils;

use crate::shared::tasks::commit_finalize_task::CommitFinalizeTask;

#[derive(Clone, Debug)]
pub enum Task {
    CommitFinalize(CommitFinalizeTask),
    Undelegate(UndelegateTask),
    BaseAction(BaseActionTask),
}

impl Task {
    pub fn involved_accounts(&self, validator: &Pubkey) -> Vec<Pubkey> {
        self.instruction(validator).accounts.iter().map(|meta| meta.pubkey).collect()
    }

    pub fn instruction(&self, validator: &Pubkey) -> Instruction {
        match self {
            Self::CommitFinalize(value) => value.instruction(validator),
            Self::Undelegate(value) => value.instruction(validator),
            Self::BaseAction(value) => value.instruction(validator),
        }
    }

    /// Switches inline commit data to buffer delivery. Returns whether the task changed.
    pub fn try_use_buffer(&mut self) -> bool {
        match self {
            Self::CommitFinalize(value) => value.try_use_buffer(),
            _ => false,
        }
    }

    pub fn compute_units(&self) -> u32 {
        match self {
            Self::CommitFinalize(value) => value.compute_units(),
            Self::BaseAction(value) => value.compute_units(),
            Self::Undelegate(_) => 120_000,
        }
    }

    pub fn accounts_size_budget(&self) -> u32 {
        match self {
            Self::CommitFinalize(value) => value.accounts_size_budget(),
            Self::BaseAction(value) => value.accounts_size_budget(),
            Self::Undelegate(value) => {
                if value.include_undelegation_request {
                    delegation_program_api::instruction_builder::undelegate_with_request_size_budget(
                        AccountSizeClass::Huge,
                    )
                } else {
                    delegation_program_api::instruction_builder::undelegate_size_budget(
                        AccountSizeClass::Huge,
                    )
                }
            }
        }
    }
}

impl LabelValue for Task {
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
            delegation_program_api::instruction_builder::undelegate_with_request(
                *validator,
                self.delegated_account,
                self.owner_program,
                self.rent_reimbursement,
            )
        } else {
            delegation_program_api::instruction_builder::undelegate(
                *validator,
                self.delegated_account,
                self.owner_program,
                self.rent_reimbursement,
            )
        }
    }
}

impl From<UndelegateTask> for Task {
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
        let action = match self {
            BaseActionTask::V1(value) => &mut value.action,
            BaseActionTask::V2(value) => &mut value.action,
        };
        let callback = action.callback.take()?;
        // Recheck persisted actions before discarding their authenticated source
        // and handing the response to the validator's signing service. The V2
        // task's separate source_program is not callback authorization.
        if action.validate_callback_destination(&callback.destination_program).is_err() {
            tracing::warn!(
                action_id = action.id,
                source_program = ?action.source_program,
                destination_program = %callback.destination_program,
                "Rejected callback destination outside its scheduling source"
            );
            return None;
        }
        Some(callback)
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
        let other_accounts_budget =
            action.account_metas_per_program.len() as u32 * AccountSizeClass::Small.size_budget();

        match self {
            Self::V1(_) => delegation_program_api::instruction_builder::call_handler_size_budget(
                AccountSizeClass::Medium,
                other_accounts_budget,
            ),
            Self::V2(_) => {
                delegation_program_api::instruction_builder::call_handler_v2_size_budget(
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
        delegation_program_api::instruction_builder::call_handler(
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

impl From<BaseActionTask> for Task {
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
        delegation_program_api::instruction_builder::call_handler_v2(
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
    use delegation_program_api::{
        discriminator::DlpDiscriminator, pda::undelegation_request_pda_from_delegated_account,
    };
    use mbv_core::intent::ProgramArgs;
    use solana_compute_budget_interface::ComputeBudgetInstruction;
    use solana_keypair::Keypair;
    use solana_signer::Signer;
    use solana_transaction::Transaction;
    use tracing::info;

    use super::*;
    use crate::{
        shared::{
            tasks::{
                buffer_task::BufferCleanup, task_strategist::TransactionStrategy,
                utils::TransactionUtils,
            },
            transaction::{MAX_TRANSACTION_WIRE_SIZE, serialized_transaction_size},
        },
        test_utils,
    };

    /// Only A-to-A responses reach the signing scheduler, even for persisted
    /// mismatches, missing provenance, or a separately supplied V2 source B.
    #[test]
    fn test_callback_destination_bound_to_recorded_source() {
        let source = Pubkey::new_unique();
        let other = Pubkey::new_unique();
        for (recorded_source, destination, accepted) in
            [(Some(source), source, true), (Some(source), other, false), (None, other, false)]
        {
            let callback = BaseActionCallback {
                destination_program: destination,
                discriminator: vec![1],
                payload: vec![2],
                compute_units: 10_000,
                account_metas_per_program: vec![],
            };
            let action = BaseAction {
                id: 0,
                compute_units: 10_000,
                // The base action may still target B.
                destination_program: other,
                source_program: recorded_source,
                escrow_authority: Pubkey::new_unique(),
                data_per_program: ProgramArgs { data: vec![], escrow_index: 0 },
                account_metas_per_program: vec![],
                callback: Some(callback.clone()),
            };
            for task in [
                BaseActionTask::V1(BaseActionTaskV1 { action: action.clone() }),
                BaseActionTask::V2(BaseActionTaskV2 { action, source_program: other }),
            ] {
                let mut strategy = TransactionStrategy {
                    tasks: vec![Task::BaseAction(task)],
                    ..Default::default()
                };
                let callbacks = strategy.extract_action_callbacks();
                // An empty extraction prevents any response transaction to B
                // from reaching the validator's callback signing service.
                assert_eq!(
                    callbacks,
                    if accepted { vec![callback.clone()] } else { vec![] }
                );
                assert!(!strategy.has_actions_callbacks());
                assert!(strategy.extract_action_callbacks().is_empty());
            }
        }
    }

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

        assert_eq!(ix.program_id, delegation_program_api::id());
        assert_eq!(ix.data, DlpDiscriminator::Undelegate.to_vec());
        assert_eq!(ix.accounts.len(), 13);
        assert_eq!(
            ix.accounts[12].pubkey,
            undelegation_request_pda_from_delegated_account(&delegated_account)
        );
        assert!(ix.accounts[12].is_writable);
        assert!(!ix.accounts[12].is_signer);
    }

    #[test]
    fn test_close_buffer_limit() {
        test_utils::init_test_logger();

        let authority = Keypair::new();

        // Budget ixs (fixed)
        let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(30_000);
        let compute_unit_price_ix = ComputeBudgetInstruction::set_compute_unit_price(101);

        // Each task unique: commit_id increments; pubkey is new_unique each time
        let base_commit_id = 101u64;
        let ixs_iter = (0..BufferCleanup::MAX_PER_TRANSACTION).map(|i| {
            let task = BufferCleanup {
                commit_id: base_commit_id + i as u64,
                pubkey: Pubkey::new_unique(),
            };
            task.instruction(&authority.pubkey())
        });

        let mut ixs: Vec<_> =
            [compute_budget_ix, compute_unit_price_ix].into_iter().chain(ixs_iter).collect();
        ixs.push(TransactionUtils::uniqueness_noop_instruction(42));

        let tx = Transaction::new_with_payer(&ixs, Some(&authority.pubkey()));
        let tx_size = serialized_transaction_size(&tx);
        info!(transaction_size = tx_size, "Cleanup task transaction size");
        assert!(tx_size <= MAX_TRANSACTION_WIRE_SIZE);

        // One more unique task should overflow
        let overflow_task = BufferCleanup {
            commit_id: base_commit_id + BufferCleanup::MAX_PER_TRANSACTION as u64,
            pubkey: Pubkey::new_unique(),
        };
        let uniqueness_noop = ixs.pop().expect("uniqueness noop");
        ixs.push(overflow_task.instruction(&authority.pubkey()));
        ixs.push(uniqueness_noop);

        let tx = Transaction::new_with_payer(&ixs, Some(&authority.pubkey()));
        assert!(serialized_transaction_size(&tx) > MAX_TRANSACTION_WIRE_SIZE);
    }
}
