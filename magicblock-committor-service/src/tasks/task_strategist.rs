use std::collections::BinaryHeap;

use magicblock_core::intent::BaseActionCallback;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::{Signer, SignerError};
use tracing::error;

use crate::{
    persist::{CommitStrategy, IntentPersister},
    tasks::{
        commit_task::CommitDelivery, utils::TransactionUtils, BaseActionTask,
        BaseTask, BaseTaskImpl,
    },
    transactions::{
        serialized_transaction_size, MAX_TRANSACTION_V1_WIRE_SIZE,
        MAX_TRANSACTION_WIRE_SIZE,
    },
};

#[derive(Default, Debug)]
pub struct TransactionStrategy {
    pub optimized_tasks: Vec<BaseTaskImpl>,
    pub lookup_tables_keys: Vec<Pubkey>,
    // TODO(edwin): remove this
    pub uniqueness_nonce: Option<u64>,
}

impl TransactionStrategy {
    /// In case old strategy used ALTs recalculate old value
    /// NOTE: this can be used when full revaluation is unnecessary, like:
    /// some tasks were reset, number of tasks didn't increase
    pub fn dummy_revaluate_alts(&mut self, authority: &Pubkey) -> Vec<Pubkey> {
        if self.lookup_tables_keys.is_empty() {
            vec![]
        } else {
            std::mem::replace(
                &mut self.lookup_tables_keys,
                TaskStrategist::collect_lookup_table_keys(
                    authority,
                    &self.optimized_tasks,
                    self.uniqueness_nonce,
                ),
            )
        }
    }

    /// Extracts callbacks from actions
    pub fn extract_action_callbacks(&mut self) -> Vec<BaseActionCallback> {
        self.optimized_tasks
            .iter_mut()
            .filter_map(|el| {
                if let BaseTaskImpl::BaseAction(value) = el {
                    Some(value)
                } else {
                    None
                }
            })
            .filter_map(BaseActionTask::extract_callback)
            .collect()
    }

    /// Handles actions error, stripping away actions
    /// Returns [`TransactionStrategy`] to be cleaned up
    pub fn remove_actions(
        &mut self,
        authority: &Pubkey,
    ) -> TransactionStrategy {
        // Strip away actions
        let (optimized_tasks, action_tasks) = self
            .optimized_tasks
            .drain(..)
            .partition(|el| !matches!(el, BaseTaskImpl::BaseAction(_)));
        self.optimized_tasks = optimized_tasks;

        let old_alts = self.dummy_revaluate_alts(authority);

        TransactionStrategy {
            optimized_tasks: action_tasks,
            lookup_tables_keys: old_alts,
            uniqueness_nonce: self.uniqueness_nonce,
        }
    }

    pub fn has_actions_callbacks(&self) -> bool {
        self.optimized_tasks
            .iter()
            .filter_map(|el| {
                if let BaseTaskImpl::BaseAction(value) = el {
                    Some(value)
                } else {
                    None
                }
            })
            .any(BaseActionTask::has_callback)
    }

    /// Task transactions use v0 with ALTs and v1 without them; see
    /// [`TaskStrategist::build_strategy`]. V0 prepends compute-budget
    /// instructions, while v1 stores budgets in its configuration.
    pub(crate) fn task_instruction_offset(&self) -> u8 {
        if self.uses_alts() {
            // In our design, ALTS implies transaction v0 which in turn implies
            // compute-budget instructions are passed explicitly
            TransactionUtils::COMPUTE_BUDGET_INSTRUCTION_COUNT
        } else {
            0
        }
    }

    pub fn uses_alts(&self) -> bool {
        !self.lookup_tables_keys.is_empty()
    }
}

pub enum StrategyExecutionMode {
    SingleStage(TransactionStrategy),
    TwoStage {
        commit_stage: TransactionStrategy,
        finalize_stage: TransactionStrategy,
    },
}

impl StrategyExecutionMode {
    pub fn uses_alts(&self) -> bool {
        match self {
            Self::SingleStage(value) => value.uses_alts(),
            Self::TwoStage {
                commit_stage,
                finalize_stage,
            } => commit_stage.uses_alts() || finalize_stage.uses_alts(),
        }
    }
}

/// Takes [`BaseTask`]s and chooses the best way to fit them in TX
/// It may change Task execution strategy so all task would fit in tx
pub struct TaskStrategist;
impl TaskStrategist {
    /// Builds execution strategy from [`BaseTask`]s
    /// 1. Optimizes tasks to fit in TX
    /// 2. Chooses the fastest execution mode for Tasks
    ///
    /// `uniqueness_nonce` is rendered as a constant-size noop instruction on
    /// every produced strategy, and is accounted for in all fit decisions.
    pub fn build_execution_strategy<P: IntentPersister>(
        commit_tasks: Vec<BaseTaskImpl>,
        finalize_tasks: Vec<BaseTaskImpl>,
        authority: &Pubkey,
        persister: &Option<P>,
        uniqueness_nonce: Option<u64>,
    ) -> TaskStrategistResult<StrategyExecutionMode> {
        const MAX_UNITED_TASKS_LEN: usize = 22;

        // Combined commits need no finalization transaction. Splitting an
        // empty second stage cannot make the commit transaction smaller.
        if finalize_tasks.is_empty()
            && commit_tasks
                .iter()
                .any(|task| matches!(task, BaseTaskImpl::CommitFinalize(_)))
        {
            return Self::build_strategy(
                commit_tasks,
                authority,
                persister,
                uniqueness_nonce,
            )
            .map(StrategyExecutionMode::SingleStage);
        }

        // We can unite in 1 tx a lot of commits
        // but then there's a possibility of hitting CPI limit, aka
        // MaxInstructionTraceLengthExceeded error.
        // So we limit tasks len with 22 total tasks
        // In case this fails as well, it will be retried with TwoStage approach
        // on retry, once retries are introduced
        if commit_tasks.len() + finalize_tasks.len() > MAX_UNITED_TASKS_LEN {
            return Self::build_two_stage(
                commit_tasks,
                finalize_tasks,
                authority,
                persister,
                uniqueness_nonce,
            );
        }

        // Clone tasks since strategies applied to united case maybe suboptimal for regular one
        // Unite tasks to attempt running as single tx
        let single_stage_tasks =
            [commit_tasks.clone(), finalize_tasks.clone()].concat();
        let single_stage_strategy = match TaskStrategist::build_strategy(
            single_stage_tasks,
            authority,
            persister,
            uniqueness_nonce,
        ) {
            Ok(strategy) => StrategyExecutionMode::SingleStage(strategy),
            Err(TaskStrategistError::FailedToFitError) => {
                // If Tasks can't fit in SingleStage - use TwpStage execution
                return Self::build_two_stage(
                    commit_tasks,
                    finalize_tasks,
                    authority,
                    persister,
                    uniqueness_nonce,
                );
            }
            Err(TaskStrategistError::SignerError(err)) => {
                return Err(err.into())
            }
        };

        // If ALTs aren't used then we sure this will be optimal - return
        if !single_stage_strategy.uses_alts() {
            return Ok(single_stage_strategy);
        }

        // As ALTs take a very long time to activate
        // it is actually faster to execute in TwoStage mode
        // unless TwoStage also uses ALTs
        let two_stage = Self::build_two_stage(
            commit_tasks,
            finalize_tasks,
            authority,
            persister,
            uniqueness_nonce,
        )?;
        if two_stage.uses_alts() {
            Ok(single_stage_strategy)
        } else {
            Ok(two_stage)
        }
    }

    fn build_two_stage<P: IntentPersister>(
        commit_tasks: Vec<BaseTaskImpl>,
        finalize_tasks: Vec<BaseTaskImpl>,
        authority: &Pubkey,
        persister: &Option<P>,
        uniqueness_nonce: Option<u64>,
    ) -> TaskStrategistResult<StrategyExecutionMode> {
        // Build strategy for Commit stage
        let commit_strategy = TaskStrategist::build_strategy(
            commit_tasks,
            authority,
            persister,
            uniqueness_nonce,
        )?;

        // Build strategy for Finalize stage
        let finalize_strategy = TaskStrategist::build_strategy(
            finalize_tasks,
            authority,
            persister,
            uniqueness_nonce,
        )?;

        Ok(StrategyExecutionMode::TwoStage {
            commit_stage: commit_strategy,
            finalize_stage: finalize_strategy,
        })
    }

    /// Builds a task transaction strategy, trying these options in order:
    ///
    /// - V1 without ALTs, without buffer-accounts.
    /// - V1 without ALTs, with buffer-accounts.
    /// - V0 with ALTs, without buffer-accounts.
    /// - V0 with ALTs, with buffer-accounts.
    ///
    /// The task preparator follows this choice: no ALTs means v1;
    /// ALTs means v0. Buffer preparation and cleanup are separate
    /// paths that use v0 without ALTs.
    ///
    /// Returns an error if the tasks cannot fit within transaction limits.
    pub fn build_strategy<P: IntentPersister>(
        tasks: Vec<BaseTaskImpl>,
        validator: &Pubkey,
        persistor: &Option<P>,
        uniqueness_nonce: Option<u64>,
    ) -> TaskStrategistResult<TransactionStrategy> {
        if TransactionUtils::tasks_compute_units(&tasks) > 1_400_000 {
            return Err(TaskStrategistError::FailedToFitError);
        }

        let mut optimized_tasks = tasks.clone();
        let tx_size = Self::try_optimize_tx_size_if_needed(
            &mut optimized_tasks,
            uniqueness_nonce,
            MAX_TRANSACTION_V1_WIRE_SIZE,
        )?;

        if tx_size <= MAX_TRANSACTION_V1_WIRE_SIZE {
            return Ok(Self::build_transaction_strategy(
                optimized_tasks,
                validator,
                persistor,
                uniqueness_nonce,
                false,
            ));
        }

        // V1 can miss on structural limits such as the 64 static account-key
        // cap, not only on byte size. Give the original tasks the old v0+ALT
        // path before introducing buffer preparation and cleanup work.
        if Self::attempt_lookup_tables(&tasks, uniqueness_nonce) {
            return Ok(Self::build_transaction_strategy(
                tasks,
                validator,
                persistor,
                uniqueness_nonce,
                true,
            ));
        }

        // If the original tasks still do not fit v0+ALT, use the
        // buffer-optimized V1 probe result as the final payload-size fallback.
        if Self::attempt_lookup_tables(&optimized_tasks, uniqueness_nonce) {
            return Ok(Self::build_transaction_strategy(
                optimized_tasks,
                validator,
                persistor,
                uniqueness_nonce,
                true,
            ));
        }

        Err(TaskStrategistError::FailedToFitError)
    }

    fn build_transaction_strategy<P: IntentPersister>(
        tasks: Vec<BaseTaskImpl>,
        validator: &Pubkey,
        persistor: &Option<P>,
        uniqueness_nonce: Option<u64>,
        uses_lookup_tables: bool,
    ) -> TransactionStrategy {
        if let Some(persistor) = persistor {
            Self::persist_tasks_strategy(persistor, &tasks, uses_lookup_tables);
        }

        let lookup_tables_keys = if uses_lookup_tables {
            Self::collect_lookup_table_keys(validator, &tasks, uniqueness_nonce)
        } else {
            vec![]
        };

        TransactionStrategy {
            optimized_tasks: tasks,
            lookup_tables_keys,
            uniqueness_nonce,
        }
    }

    /// Attempt to use ALTs for ALL keys in tx
    /// Returns `true` if ALTs make tx fit, otherwise `false`
    /// TODO(edwin): optimize to use only necessary amount of pubkeys
    pub fn attempt_lookup_tables(
        tasks: &[BaseTaskImpl],
        uniqueness_nonce: Option<u64>,
    ) -> bool {
        if TransactionUtils::tasks_compute_units(tasks) > 1_400_000 {
            return false;
        }

        let placeholder = Keypair::new();
        let dummy_lookup_tables = TransactionUtils::dummy_lookup_table(
            &Self::collect_lookup_table_keys(
                &placeholder.pubkey(),
                tasks,
                uniqueness_nonce,
            ),
        );

        // Assemble through the same path used for the real transaction so
        // fit decisions cannot diverge from the assembled message.
        match TransactionUtils::assemble_tasks_tx_with_uniqueness_nonce(
            &placeholder,
            tasks,
            u64::default(),
            &dummy_lookup_tables,
            uniqueness_nonce,
        ) {
            Ok(tx) => {
                serialized_transaction_size(&tx) <= MAX_TRANSACTION_WIRE_SIZE
            }
            // Transaction doesn't fit, see CompileError
            Err(_) => false,
        }
    }

    pub fn collect_lookup_table_keys(
        authority: &Pubkey,
        tasks: &[BaseTaskImpl],
        uniqueness_nonce: Option<u64>,
    ) -> Vec<Pubkey> {
        let budgets = TransactionUtils::tasks_compute_units(tasks);
        let size_budgets = TransactionUtils::tasks_accounts_size_budget(tasks);
        let mut service_instructions = TransactionUtils::budget_instructions(
            budgets,
            u64::default(),
            size_budgets,
        )
        .to_vec();
        if let Some(nonce) = uniqueness_nonce {
            service_instructions
                .push(TransactionUtils::uniqueness_noop_instruction(nonce));
        }

        TransactionUtils::unique_involved_pubkeys(
            tasks,
            authority,
            &service_instructions,
        )
    }

    fn persist_tasks_strategy<P: IntentPersister>(
        persistor: &P,
        tasks: &[BaseTaskImpl],
        uses_lookup_tables: bool,
    ) {
        let commit_strategy_from_delivery =
            |delivery: &CommitDelivery| match delivery {
                CommitDelivery::StateInArgs => {
                    if uses_lookup_tables {
                        CommitStrategy::StateArgsWithLookupTable
                    } else {
                        CommitStrategy::StateArgs
                    }
                }
                CommitDelivery::DiffInArgs { .. } => {
                    if uses_lookup_tables {
                        CommitStrategy::DiffArgsWithLookupTable
                    } else {
                        CommitStrategy::DiffArgs
                    }
                }
                CommitDelivery::StateInBuffer { .. } => {
                    if uses_lookup_tables {
                        CommitStrategy::StateBufferWithLookupTable
                    } else {
                        CommitStrategy::StateBuffer
                    }
                }
                CommitDelivery::DiffInBuffer { .. } => {
                    if uses_lookup_tables {
                        CommitStrategy::DiffBufferWithLookupTable
                    } else {
                        CommitStrategy::DiffBuffer
                    }
                }
            };

        for task in tasks {
            let (commit_id, pubkey, commit_strategy) = match task {
                BaseTaskImpl::Commit(commit_task) => (
                    commit_task.commit_id,
                    commit_task.committed_account.pubkey,
                    commit_strategy_from_delivery(
                        &commit_task.delivery_details,
                    ),
                ),
                BaseTaskImpl::CommitFinalize(commit_finalize_task) => (
                    commit_finalize_task.commit_id,
                    commit_finalize_task.committed_account.pubkey,
                    commit_strategy_from_delivery(
                        &commit_finalize_task.delivery,
                    ),
                ),
                _ => continue,
            };
            if let Err(err) = persistor.set_commit_strategy(
                commit_id,
                &pubkey,
                commit_strategy,
            ) {
                error!(
                    commit_id = %commit_id,
                    pubkey = %pubkey,
                    strategy = commit_strategy.as_str(),
                    error = ?err,
                    "Failed to persist commit strategy"
                );
            }
        }
    }

    /// Optimizes tasks so as to bring the transaction size within the requested limit.
    /// Returns Ok(size of tx after optimizations) else Err(SignerError).
    /// Note that the returned size, though possibly optimized one, may still not be under
    /// the requested limit. The caller needs to check and make decision accordingly.
    fn try_optimize_tx_size_if_needed(
        tasks: &mut [BaseTaskImpl],
        uniqueness_nonce: Option<u64>,
        tx_size_limit: usize,
    ) -> Result<usize, SignerError> {
        // Get initial transaction size
        let calculate_tx_length = |tasks: &[BaseTaskImpl]| {
            // Include the constant-size uniqueness noop so fit decisions
            // match the assembled transaction.
            match TransactionUtils::assemble_tasks_v1_tx_with_uniqueness_nonce(
                &Keypair::new(), // placeholder
                tasks,
                u64::default(), // placeholder
                uniqueness_nonce,
            ) {
                Ok(tx) => Ok(tx.serialized_size()),
                Err(TaskStrategistError::FailedToFitError) => Ok(usize::MAX),
                Err(TaskStrategistError::SignerError(err)) => Err(err),
            }
        };

        // Get initial transaction size
        let mut current_tx_length = calculate_tx_length(tasks)?;

        if current_tx_length <= tx_size_limit {
            return Ok(current_tx_length);
        }

        // Create heap size -> index
        let ixs =
            TransactionUtils::tasks_instructions(&Pubkey::new_unique(), tasks);
        // Possible serialization failures are possible only due to size in our case
        // In that case we set size to max
        let sizes = ixs
            .iter()
            .map(|ix| bincode::serialized_size(ix).unwrap_or(u64::MAX))
            .map(|size| usize::try_from(size).unwrap_or(usize::MAX))
            .collect::<Vec<_>>();
        let mut map = sizes
            .into_iter()
            .enumerate()
            .map(|(index, size)| (size, index))
            .collect::<BinaryHeap<_>>();

        // We keep popping heaviest el-ts & try to optimize while heap is non-empty
        while let Some((_, index)) = map.pop() {
            if current_tx_length <= tx_size_limit {
                break;
            }

            if tasks[index].try_optimize_tx_size() {
                // If we can decrease:
                // 1. Calculate new tx size & ix size
                // 2. Insert item's data back in the heap
                // 3. Update overall tx size
                let new_ix = tasks[index].instruction(&Pubkey::new_unique());
                // Possible serialization failures are possible only due to size in our case
                // In that case we set size to max
                let new_ix_size =
                    bincode::serialized_size(&new_ix).unwrap_or(u64::MAX);
                let new_ix_size =
                    usize::try_from(new_ix_size).unwrap_or(usize::MAX);
                current_tx_length = calculate_tx_length(tasks)?;
                map.push((new_ix_size, index));
            }
        }

        Ok(current_tx_length)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TaskStrategistError {
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    #[error("SignerError: {0}")]
    SignerError(#[from] SignerError),
}

pub type TaskStrategistResult<T, E = TaskStrategistError> = Result<T, E>;

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use dlp_api::{
        discriminator::DlpDiscriminator,
        state::{DelegationMetadata, UndelegationRequester},
    };
    use magicblock_core::intent::{
        types::CommittedAccount, BaseAction, CommitAndUndelegate, CommitType,
        MagicBaseIntent, ProgramArgs, UndelegateType,
    };
    use magicblock_program::args::ShortAccountMeta;
    use solana_account::Account;
    use solana_pubkey::Pubkey;

    use super::*;
    use crate::{
        intent_execution_manager::intent_scheduler::create_test_intent,
        intent_executor::task_info_fetcher::{
            AccountSnapshot, TaskInfoFetcher, TaskInfoFetcherResult,
        },
        persist::IntentPersisterImpl,
        tasks::{
            commit_task::CommitTask,
            task_builder::{TaskBuilderImpl, TasksBuilder},
            utils::{create_commit_task, COMMIT_STATE_SIZE_THRESHOLD},
            BaseActionTask, BaseActionTaskV1, FinalizeTask, TaskStrategy,
            UndelegateTask,
        },
        test_utils,
    };

    #[derive(Default)]
    struct MockInfoFetcher {
        delegation_metadata: HashMap<Pubkey, (UndelegationRequester, Pubkey)>,
        base_accounts: HashMap<Pubkey, Account>,
    }

    #[async_trait::async_trait]
    impl TaskInfoFetcher for MockInfoFetcher {
        async fn fetch_next_commit_nonces(
            &self,
            accounts: &[AccountSnapshot],
            _: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            Ok(accounts.iter().map(|(pubkey, _)| (*pubkey, 0)).collect())
        }

        async fn fetch_current_commit_nonces(
            &self,
            accounts: &[AccountSnapshot],
            _: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            Ok(accounts.iter().map(|(pubkey, _)| (*pubkey, 0)).collect())
        }

        async fn fetch_delegation_metadata(
            &self,
            accounts: &[AccountSnapshot],
            _: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, DelegationMetadata>>
        {
            Ok(accounts
                .iter()
                .map(|(pubkey, _)| {
                    let (undelegation_requester, rent_payer) = self
                        .delegation_metadata
                        .get(pubkey)
                        .copied()
                        .unwrap_or((UndelegationRequester::None, *pubkey));
                    (
                        *pubkey,
                        DelegationMetadata {
                            last_commit_id: 0,
                            undelegation_requester,
                            seeds: vec![],
                            rent_payer,
                        },
                    )
                })
                .collect())
        }

        async fn get_base_accounts(
            &self,
            _pubkeys: &[Pubkey],
            _: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, Account>> {
            Ok(self.base_accounts.clone())
        }
    }

    // Helper to create a simple commit task
    fn create_test_commit_task(
        commit_id: u64,
        data_size: usize,
        diff_len: usize,
    ) -> CommitTask {
        let committed_account = CommittedAccount {
            pubkey: Pubkey::new_unique(),
            account: Account {
                lamports: 1000,
                data: vec![1; data_size],
                ..Default::default()
            },
            remote_slot: Default::default(),
        };

        if diff_len == 0 {
            create_commit_task(commit_id, false, committed_account, None)
        } else {
            let base_account = {
                let mut acc = committed_account.account.clone();
                assert!(diff_len <= acc.data.len());
                for byte in &mut acc.data[..diff_len] {
                    *byte = byte.wrapping_add(1);
                }
                acc
            };
            create_commit_task(
                commit_id,
                false,
                committed_account,
                Some(base_account),
            )
        }
    }

    // Helper to create a Base action task
    fn create_test_base_action_task(len: usize) -> BaseActionTask {
        BaseActionTaskV1 {
            action: BaseAction {
                id: 0,
                destination_program: Pubkey::new_unique(),
                source_program: None,
                escrow_authority: Pubkey::new_unique(),
                account_metas_per_program: vec![],
                data_per_program: ProgramArgs {
                    data: vec![0; len],
                    escrow_index: 0,
                },
                compute_units: 30_000,
                callback: None,
            },
        }
        .into()
    }

    // Helper to create a finalize task
    fn create_test_finalize_task() -> FinalizeTask {
        FinalizeTask {
            delegated_account: Pubkey::new_unique(),
        }
    }

    // Helper to create an undelegate task
    fn create_test_undelegate_task() -> UndelegateTask {
        UndelegateTask {
            delegated_account: Pubkey::new_unique(),
            owner_program: Pubkey::default(),
            rent_reimbursement: Pubkey::new_unique(),
            include_undelegation_request: false,
        }
    }

    #[test]
    fn test_build_strategy_with_single_small_task() {
        test_utils::init_test_logger();
        let validator = Pubkey::new_unique();
        let task = create_test_commit_task(1, 100, 0);
        let tasks = vec![task.into()];

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
            None,
        )
        .expect("Should build strategy");

        assert_eq!(strategy.optimized_tasks.len(), 1);
        assert!(strategy.lookup_tables_keys.is_empty());
    }

    #[test]
    fn test_build_strategy_optimizes_to_buffer_when_needed() {
        let validator = Pubkey::new_unique();

        let task = create_test_commit_task(1, 5000, 0); // Large task
        let tasks = vec![task.into()];

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
            None,
        )
        .expect("Should build strategy with buffer optimization");

        assert_eq!(strategy.optimized_tasks.len(), 1);
        assert!(matches!(
            strategy.optimized_tasks[0].strategy(),
            TaskStrategy::Buffer
        ));
    }

    #[test]
    fn test_build_strategy_optimizes_to_buffer_u16_exceeded() {
        let validator = Pubkey::new_unique();

        let task = create_test_commit_task(1, 66_000, 0); // Large task
        let tasks = vec![task.into()];

        let result = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
            None,
        );

        assert!(matches!(result, Err(TaskStrategistError::FailedToFitError)));
    }

    #[test]
    fn test_build_strategy_does_not_optimize_large_account_but_small_diff() {
        let validator = Pubkey::new_unique();

        let task =
            create_test_commit_task(1, 10_240, COMMIT_STATE_SIZE_THRESHOLD); // large account but small diff
        let tasks = vec![task.into()];

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
            None,
        )
        .expect("Should build strategy with buffer optimization");

        assert_eq!(strategy.optimized_tasks.len(), 1);
        assert_eq!(strategy.optimized_tasks[0].strategy(), TaskStrategy::Args);
    }

    #[test]
    fn test_build_strategy_does_not_optimize_large_account_and_above_threshold_diff(
    ) {
        let validator = Pubkey::new_unique();

        let task =
            create_test_commit_task(1, 10_240, COMMIT_STATE_SIZE_THRESHOLD + 1); // large account but small diff
        let tasks = vec![task.into()];

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
            None,
        )
        .expect("Should build strategy with buffer optimization");

        assert_eq!(strategy.optimized_tasks.len(), 1);
        assert_eq!(strategy.optimized_tasks[0].strategy(), TaskStrategy::Args);
    }

    #[test]
    fn test_build_strategy_does_optimize_large_account_and_large_diff() {
        let validator = Pubkey::new_unique();

        let task = create_test_commit_task(1, 10_240, 5000);
        let tasks = vec![task.into()];

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
            None,
        )
        .expect("Should build strategy with buffer optimization");

        assert_eq!(strategy.optimized_tasks.len(), 1);
        assert_eq!(
            strategy.optimized_tasks[0].strategy(),
            TaskStrategy::Buffer
        );
    }

    #[test]
    fn test_build_strategy_creates_multiple_buffers() {
        // TODO: ALSO MAX NUM WITH PURE BUFFER commits, no alts
        const NUM_COMMITS: u64 = 3;

        let validator = Pubkey::new_unique();

        let tasks = (0..NUM_COMMITS)
            .map(|i| {
                let task = create_test_commit_task(i, 5000, 0); // Large task
                task.into()
            })
            .collect();

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
            None,
        )
        .expect("Should build strategy with buffer optimization");

        for optimized_task in strategy.optimized_tasks {
            assert!(matches!(optimized_task.strategy(), TaskStrategy::Buffer));
        }
        assert!(strategy.lookup_tables_keys.is_empty());
    }

    #[test]
    fn test_build_strategy_with_lookup_tables_when_needed() {
        // Also max number of committed accounts fit with ALTs!
        const NUM_COMMITS: u64 = 11;

        let validator = Pubkey::new_unique();

        let tasks = (0..NUM_COMMITS)
            .map(|i| {
                // Large task
                let task = create_test_commit_task(i, 1000, 0);
                task.into()
            })
            .collect();

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
            None,
        )
        .expect("Should build strategy with buffer optimization");

        for optimized_task in strategy.optimized_tasks {
            assert!(matches!(optimized_task.strategy(), TaskStrategy::Buffer));
        }
        assert!(!strategy.lookup_tables_keys.is_empty());
    }

    #[test]
    fn test_build_strategy_reserves_space_for_uniqueness_nonce() {
        let validator = Pubkey::new_unique();
        let build = |data_len, nonce| {
            TaskStrategist::build_strategy(
                vec![create_test_base_action_task(data_len).into()],
                &validator,
                &None::<IntentPersisterImpl>,
                nonce,
            )
        };

        let no_nonce_boundary_len = (0..MAX_TRANSACTION_V1_WIRE_SIZE)
            .rev()
            .find(|data_len| {
                build(*data_len, None).is_ok()
                    && matches!(
                        build(*data_len, Some(42)),
                        Err(TaskStrategistError::FailedToFitError)
                    )
            })
            .expect("expected a payload size at the v1 boundary");
        let nonce_boundary_len = (0..no_nonce_boundary_len)
            .rev()
            .find(|data_len| build(*data_len, Some(42)).is_ok())
            .expect("expected a nonce-bearing payload size to fit");

        let strategy = build(nonce_boundary_len, Some(42))
            .expect("should fit below the nonce boundary");
        assert_eq!(strategy.uniqueness_nonce, Some(42));
    }

    #[test]
    fn test_build_strategy_tries_v0_alts_before_buffering_v1_key_overflow() {
        let validator = Pubkey::new_unique();
        let action: BaseActionTask = BaseActionTaskV1 {
            action: BaseAction {
                id: 0,
                destination_program: Pubkey::new_unique(),
                source_program: None,
                escrow_authority: Pubkey::new_unique(),
                account_metas_per_program: (0..65)
                    .map(|_| ShortAccountMeta {
                        pubkey: Pubkey::new_unique(),
                        is_writable: false,
                    })
                    .collect(),
                data_per_program: ProgramArgs {
                    data: vec![],
                    escrow_index: 0,
                },
                compute_units: 30_000,
                callback: None,
            },
        }
        .into();

        let strategy = TaskStrategist::build_strategy(
            vec![action.into()],
            &validator,
            &None::<IntentPersisterImpl>,
            None,
        )
        .expect("should fall back to v0 + ALTs");

        assert!(!strategy.lookup_tables_keys.is_empty());
        assert!(strategy
            .optimized_tasks
            .iter()
            .all(|task| task.strategy() == TaskStrategy::Args));
    }

    #[test]
    fn test_build_strategy_fails_when_cant_fit() {
        const NUM_COMMITS: u64 = 23;

        let validator = Pubkey::new_unique();

        let tasks = (0..NUM_COMMITS)
            .map(|i| {
                // Large task
                let task = create_test_commit_task(i, 1000, 0);
                task.into()
            })
            .collect();

        let result = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
            None,
        );
        assert!(matches!(result, Err(TaskStrategistError::FailedToFitError)));
    }

    #[test]
    fn test_uniqueness_nonce_renders_distinct_noop_instruction() {
        use solana_transaction::versioned::VersionedTransaction;

        let noop_program = solana_pubkey::pubkey!(
            "noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV"
        );
        let authority = Keypair::new();
        let assemble = |nonce: Option<u64>| {
            TransactionUtils::assemble_tasks_tx_with_uniqueness_nonce(
                &authority,
                &[create_test_commit_task(1, 100, 0).into()],
                u64::default(),
                &[],
                nonce,
            )
            .expect("assembles")
        };

        let without_nonce = assemble(None);
        assert!(!without_nonce
            .message
            .static_account_keys()
            .contains(&noop_program));

        let extract_noop_data = |tx: &VersionedTransaction| {
            let keys = tx.message.static_account_keys();
            tx.message
                .instructions()
                .iter()
                .find(|ix| keys[ix.program_id_index as usize] == noop_program)
                .expect("noop instruction present")
                .data
                .clone()
        };
        assert_eq!(extract_noop_data(&assemble(Some(1))), 1u64.to_le_bytes());
        assert_eq!(extract_noop_data(&assemble(Some(2))), 2u64.to_le_bytes());
    }

    #[test]
    fn test_optimize_strategy_prioritizes_largest_tasks() {
        let mut tasks: [BaseTaskImpl; 3] = [
            create_test_commit_task(1, 100, 0).into(),
            create_test_commit_task(2, 5000, 0).into(), // Larger task
            create_test_commit_task(3, 5000, 0).into(), // Larger task
        ];

        let _ = TaskStrategist::try_optimize_tx_size_if_needed(
            &mut tasks,
            None,
            MAX_TRANSACTION_V1_WIRE_SIZE,
        );
        // The larger task should have been optimized first
        assert!(matches!(tasks[0].strategy(), TaskStrategy::Args));
        assert!(matches!(tasks[1].strategy(), TaskStrategy::Buffer));
    }

    #[test]
    fn test_mixed_task_types_with_optimization() {
        let validator = Pubkey::new_unique();
        let tasks: Vec<BaseTaskImpl> = vec![
            create_test_commit_task(1, 5000, 0).into(),
            create_test_finalize_task().into(),
            create_test_base_action_task(500).into(),
            create_test_undelegate_task().into(),
        ];

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
            None,
        )
        .expect("Should build strategy");

        assert_eq!(strategy.optimized_tasks.len(), 4);

        let strategies: Vec<TaskStrategy> = strategy
            .optimized_tasks
            .iter()
            .map(|t| t.strategy())
            .collect();

        assert_eq!(
            strategies,
            vec![
                TaskStrategy::Buffer, // Commit task optimized
                TaskStrategy::Args,   // Finalize stays
                TaskStrategy::Args,   // BaseAction stays
                TaskStrategy::Args,   // Undelegate stays
            ]
        );
        assert!(strategy.lookup_tables_keys.is_empty());
    }

    #[tokio::test]
    async fn test_finalize_tasks_include_request_for_owner_program_undelegate()
    {
        let delegated_account = Pubkey::new_unique();
        let intent = create_test_intent(0, &[delegated_account], true);
        let info_fetcher = Arc::new(MockInfoFetcher {
            delegation_metadata: HashMap::from([(
                delegated_account,
                (UndelegationRequester::OwnerProgram, delegated_account),
            )]),
            ..Default::default()
        });

        let tasks = TaskBuilderImpl::finalize_tasks(&info_fetcher, &intent)
            .await
            .unwrap();

        let BaseTaskImpl::Undelegate(task) = &tasks[0] else {
            panic!("expected undelegate task");
        };
        assert_eq!(task.delegated_account, delegated_account);
        assert_eq!(task.rent_reimbursement, delegated_account);
        assert!(task.include_undelegation_request);
    }

    #[tokio::test]
    async fn test_all_commit_intents_use_combined_tasks() {
        let pubkey = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        for use_diff in [false, true] {
            for combined in [false, true] {
                for undelegate in [false, true] {
                    let mut intent = create_test_intent(0, &[pubkey], false);
                    let mut accounts = intent
                        .intent_bundle
                        .commit
                        .take()
                        .unwrap()
                        .get_committed_accounts()
                        .clone();
                    accounts[0].account.data =
                        vec![1; if use_diff { 1024 } else { 40 }];
                    let mut base = accounts[0].account.clone();
                    base.data[0] = 0;
                    let info_fetcher = Arc::new(MockInfoFetcher {
                        base_accounts: if use_diff {
                            HashMap::from([(pubkey, base)])
                        } else {
                            HashMap::new()
                        },
                        ..Default::default()
                    });
                    let BaseActionTask::V1(action) =
                        create_test_base_action_task(0)
                    else {
                        panic!("expected v1 action");
                    };
                    let commit = CommitType::WithBaseActions {
                        committed_accounts: accounts,
                        base_actions: vec![action.action.clone()],
                    };
                    let base_intent = match (combined, undelegate) {
                        (false, false) => MagicBaseIntent::Commit(commit),
                        (true, false) => {
                            MagicBaseIntent::CommitFinalize(commit)
                        }
                        (false, true) => MagicBaseIntent::CommitAndUndelegate(
                            CommitAndUndelegate {
                                commit_action: commit,
                                undelegate_action:
                                    UndelegateType::WithBaseActions(vec![
                                        action.action,
                                    ]),
                            },
                        ),
                        (true, true) => {
                            MagicBaseIntent::CommitFinalizeAndUndelegate(
                                CommitAndUndelegate {
                                    commit_action: commit,
                                    undelegate_action:
                                        UndelegateType::WithBaseActions(vec![
                                            action.action,
                                        ]),
                                },
                            )
                        }
                    };
                    intent.intent_bundle = base_intent.into();
                    let mut commits = TaskBuilderImpl::commit_tasks(
                        &info_fetcher,
                        &intent,
                        &None::<IntentPersisterImpl>,
                    )
                    .await
                    .unwrap();
                    let finalizes =
                        TaskBuilderImpl::finalize_tasks(&info_fetcher, &intent)
                            .await
                            .unwrap();
                    assert_eq!(commits.len(), 1 + usize::from(combined));
                    let BaseTaskImpl::CommitFinalize(task) = &mut commits[0]
                    else {
                        panic!("all commit intents must use CommitFinalize");
                    };
                    assert_eq!(task.allow_undelegation, undelegate);
                    assert_eq!(task.committed_account.pubkey, pubkey);
                    assert_eq!(
                        matches!(
                            task.delivery,
                            CommitDelivery::DiffInArgs { .. }
                        ),
                        use_diff
                    );
                    let inline_ix = task.instruction(&authority);
                    assert!(task.try_optimize_tx_size());
                    let buffer_ix = task.instruction(&authority);
                    assert!(inline_ix.data.starts_with(
                        &DlpDiscriminator::CommitFinalize.to_vec()
                    ));
                    assert!(buffer_ix.data.starts_with(
                        &DlpDiscriminator::CommitFinalizeFromBuffer.to_vec()
                    ));
                    assert_eq!(inline_ix.program_id, dlp_api::id());
                    assert_eq!(buffer_ix.program_id, dlp_api::id());
                    assert_eq!(
                        finalizes.len(),
                        usize::from(!combined) + 2 * usize::from(undelegate)
                    );
                    let mut tail = finalizes.iter();
                    if !combined {
                        assert!(matches!(
                            tail.next(),
                            Some(BaseTaskImpl::BaseAction(_))
                        ));
                    }
                    if undelegate {
                        assert!(matches!(
                            tail.next(),
                            Some(BaseTaskImpl::Undelegate(_))
                        ));
                        assert!(matches!(
                            tail.next(),
                            Some(BaseTaskImpl::BaseAction(_))
                        ));
                    }
                    assert!(tail.next().is_none());
                }
            }
        }
    }

    #[tokio::test]
    async fn test_combined_commit_delivery_formats_without_finalize_stage() {
        for data_len in [40, 10_240] {
            for use_alts in [false, true] {
                let mut intent =
                    create_test_intent(0, &[Pubkey::new_unique()], false);
                intent
                    .intent_bundle
                    .commit
                    .as_mut()
                    .unwrap()
                    .get_committed_accounts_mut()[0]
                    .account
                    .data = vec![1; data_len];
                let BaseActionTask::V1(mut action) =
                    create_test_base_action_task(0)
                else {
                    panic!("expected v1 action");
                };
                if use_alts {
                    action.action.account_metas_per_program = (0..65)
                        .map(|_| ShortAccountMeta {
                            pubkey: Pubkey::new_unique(),
                            is_writable: false,
                        })
                        .collect();
                }
                intent.intent_bundle.standalone_actions.push(action.action);
                let fetcher = Arc::new(MockInfoFetcher::default());
                let commits = TaskBuilderImpl::commit_tasks(
                    &fetcher,
                    &intent,
                    &None::<IntentPersisterImpl>,
                )
                .await
                .unwrap();
                let finalizes =
                    TaskBuilderImpl::finalize_tasks(&fetcher, &intent)
                        .await
                        .unwrap();
                assert!(finalizes.is_empty());
                let mode = TaskStrategist::build_execution_strategy(
                    commits,
                    finalizes,
                    &Pubkey::new_unique(),
                    &None::<IntentPersisterImpl>,
                    Some(1),
                )
                .unwrap();
                let StrategyExecutionMode::SingleStage(strategy) = mode else {
                    panic!("no separate finalization transaction is needed");
                };
                assert_eq!(strategy.uses_alts(), use_alts);
                let BaseTaskImpl::CommitFinalize(task) =
                    &strategy.optimized_tasks[1]
                else {
                    panic!("format fallback must retain CommitFinalize");
                };
                assert_eq!(
                    matches!(
                        task.delivery,
                        CommitDelivery::StateInBuffer { .. }
                    ),
                    data_len > 4096
                );
            }
        }
    }

    #[tokio::test]
    async fn test_build_single_stage_mode() {
        let pubkey = [Pubkey::new_unique()];
        let intent = create_test_intent(0, &pubkey, false);

        let info_fetcher = Arc::new(MockInfoFetcher::default());
        let commit_task = TaskBuilderImpl::commit_tasks(
            &info_fetcher,
            &intent,
            &None::<IntentPersisterImpl>,
        )
        .await
        .unwrap();
        let finalize_task =
            TaskBuilderImpl::finalize_tasks(&info_fetcher, &intent)
                .await
                .unwrap();

        let execution_mode = TaskStrategist::build_execution_strategy(
            commit_task,
            finalize_task,
            &Pubkey::new_unique(),
            &None::<IntentPersisterImpl>,
            None,
        )
        .expect("Execution mode created");

        let StrategyExecutionMode::SingleStage(value) = execution_mode else {
            panic!("Unexpected execution mode");
        };
        assert!(!value.uses_alts());
    }

    #[tokio::test]
    async fn test_build_two_stage_mode_when_task_count_exceeds_single_stage_limit(
    ) {
        let mut intent = create_test_intent(0, &[], false);
        intent.intent_bundle.standalone_actions = (0..23)
            .map(|_| BaseAction {
                id: 0,
                destination_program: Pubkey::new_unique(),
                source_program: None,
                escrow_authority: Pubkey::new_unique(),
                account_metas_per_program: vec![],
                data_per_program: ProgramArgs {
                    data: vec![],
                    escrow_index: 0,
                },
                compute_units: 30_000,
                callback: None,
            })
            .collect();

        let info_fetcher = Arc::new(MockInfoFetcher::default());
        let commit_task = TaskBuilderImpl::commit_tasks(
            &info_fetcher,
            &intent,
            &None::<IntentPersisterImpl>,
        )
        .await
        .unwrap();
        let finalize_task =
            TaskBuilderImpl::finalize_tasks(&info_fetcher, &intent)
                .await
                .unwrap();

        let execution_mode = TaskStrategist::build_execution_strategy(
            commit_task,
            finalize_task,
            &Pubkey::new_unique(),
            &None::<IntentPersisterImpl>,
            None,
        )
        .expect("Execution mode created");

        let StrategyExecutionMode::TwoStage { .. } = execution_mode else {
            panic!("Unexpected execution mode");
        };
    }
}
