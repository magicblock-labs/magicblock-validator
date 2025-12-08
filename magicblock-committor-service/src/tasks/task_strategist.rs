use std::collections::BinaryHeap;

use solana_pubkey::Pubkey;
use solana_sdk::{
    signature::Keypair,
    signer::{Signer, SignerError},
};

use crate::{
    persist::IntentPersister,
    tasks::{
        args_task::{ArgsTask, ArgsTaskType},
        task_visitors::persistor_visitor::{
            PersistorContext, PersistorVisitor,
        },
        utils::TransactionUtils,
        BaseTask, FinalizeTask,
    },
    transactions::{serialize_and_encode_base64, MAX_ENCODED_TRANSACTION_SIZE},
};

pub struct TransactionStrategy {
    pub optimized_tasks: Vec<Box<dyn BaseTask>>,
    pub lookup_tables_keys: Vec<Pubkey>,
    pub compressed: bool,
}

impl TransactionStrategy {
    pub fn try_new(
        optimized_tasks: Vec<Box<dyn BaseTask>>,
        lookup_tables_keys: Vec<Pubkey>,
    ) -> Result<Self, TaskStrategistError> {
        let compressed = optimized_tasks
            .iter()
            .fold(None, |state, task| match state {
                None => Some(Ok(task.is_compressed())),
                Some(Ok(state)) if state != task.is_compressed() => {
                    Some(Err(TaskStrategistError::InconsistentTaskCompression))
                }
                Some(Ok(state)) => Some(Ok(state)),
                Some(Err(err)) => Some(Err(err)),
            })
            .unwrap_or(Ok(false))?;
        Ok(Self {
            optimized_tasks,
            lookup_tables_keys,
            compressed,
        })
    }
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
                ),
            )
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
    pub fn build_execution_strategy<P: IntentPersister>(
        commit_tasks: Vec<Box<dyn BaseTask>>,
        finalize_tasks: Vec<Box<dyn BaseTask>>,
        authority: &Pubkey,
        persister: &Option<P>,
    ) -> TaskStrategistResult<StrategyExecutionMode> {
        const MAX_UNITED_TASKS_LEN: usize = 22;

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
        ) {
            Ok(strategy) => StrategyExecutionMode::SingleStage(strategy),
            Err(TaskStrategistError::FailedToFitError) => {
                // If Tasks can't fit in SingleStage - use TwpStage execution
                return Self::build_two_stage(
                    commit_tasks,
                    finalize_tasks,
                    authority,
                    persister,
                );
            }
            Err(TaskStrategistError::SignerError(err)) => {
                return Err(err.into())
            }
            Err(TaskStrategistError::InconsistentTaskCompression) => {
                return Err(TaskStrategistError::InconsistentTaskCompression);
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
        )?;
        if two_stage.uses_alts() {
            Ok(single_stage_strategy)
        } else {
            Ok(two_stage)
        }
    }

    fn build_two_stage<P: IntentPersister>(
        commit_tasks: Vec<Box<dyn BaseTask>>,
        finalize_tasks: Vec<Box<dyn BaseTask>>,
        authority: &Pubkey,
        persister: &Option<P>,
    ) -> TaskStrategistResult<StrategyExecutionMode> {
        // Build strategy for Commit stage
        let commit_strategy =
            TaskStrategist::build_strategy(commit_tasks, authority, persister)?;

        // Build strategy for Finalize stage
        let finalize_strategy = TaskStrategist::build_strategy(
            finalize_tasks,
            authority,
            persister,
        )?;

        Ok(StrategyExecutionMode::TwoStage {
            commit_stage: commit_strategy,
            finalize_stage: finalize_strategy,
        })
    }

    /// Returns [`TransactionStrategy`] for tasks
    /// Returns Error if all optimizations weren't enough
    pub fn build_strategy<P: IntentPersister>(
        mut tasks: Vec<Box<dyn BaseTask>>,
        validator: &Pubkey,
        persistor: &Option<P>,
    ) -> TaskStrategistResult<TransactionStrategy> {
        // Attempt optimizing tasks themselves(using buffers)
        if Self::optimize_strategy(&mut tasks)? <= MAX_ENCODED_TRANSACTION_SIZE
        {
            // Persist tasks strategy
            if let Some(persistor) = persistor {
                let mut persistor_visitor = PersistorVisitor {
                    persistor,
                    context: PersistorContext::PersistStrategy {
                        uses_lookup_tables: false,
                    },
                };
                tasks
                    .iter()
                    .for_each(|task| task.visit(&mut persistor_visitor));
            }

            TransactionStrategy::try_new(tasks, vec![])
        }
        // In case task optimization didn't work
        // attempt using lookup tables for all keys involved in tasks
        else if Self::attempt_lookup_tables(&tasks) {
            // Persist tasks strategy
            let mut persistor_visitor = PersistorVisitor {
                persistor,
                context: PersistorContext::PersistStrategy {
                    uses_lookup_tables: true,
                },
            };
            tasks
                .iter()
                .for_each(|task| task.visit(&mut persistor_visitor));

            // Get lookup table keys
            let lookup_tables_keys =
                Self::collect_lookup_table_keys(validator, &tasks);
            TransactionStrategy::try_new(tasks, lookup_tables_keys)
        } else {
            Err(TaskStrategistError::FailedToFitError)
        }
    }

    /// Attempt to use ALTs for ALL keys in tx
    /// Returns `true` if ALTs make tx fit, otherwise `false`
    /// TODO(edwin): optimize to use only necessary amount of pubkeys
    pub fn attempt_lookup_tables(tasks: &[Box<dyn BaseTask>]) -> bool {
        let placeholder = Keypair::new();
        // Gather all involved keys in tx
        let budgets = TransactionUtils::tasks_compute_units(tasks);
        let budget_instructions =
            TransactionUtils::budget_instructions(budgets, u64::default());
        let unique_involved_pubkeys = TransactionUtils::unique_involved_pubkeys(
            tasks,
            &placeholder.pubkey(),
            &budget_instructions,
        );
        let dummy_lookup_tables =
            TransactionUtils::dummy_lookup_table(&unique_involved_pubkeys);

        // Create final tx
        let instructions =
            TransactionUtils::tasks_instructions(&placeholder.pubkey(), tasks);
        let alt_tx = if let Ok(tx) = TransactionUtils::assemble_tx_raw(
            &placeholder,
            &instructions,
            &budget_instructions,
            &dummy_lookup_tables,
        ) {
            tx
        } else {
            // Transaction doesn't fit, see CompileError
            return false;
        };

        let encoded_alt_tx = serialize_and_encode_base64(&alt_tx);
        encoded_alt_tx.len() <= MAX_ENCODED_TRANSACTION_SIZE
    }

    pub fn collect_lookup_table_keys(
        authority: &Pubkey,
        tasks: &[Box<dyn BaseTask>],
    ) -> Vec<Pubkey> {
        let budgets = TransactionUtils::tasks_compute_units(tasks);
        let budget_instructions =
            TransactionUtils::budget_instructions(budgets, u64::default());

        TransactionUtils::unique_involved_pubkeys(
            tasks,
            authority,
            &budget_instructions,
        )
    }

    /// Optimizes set of [`TaskDeliveryStrategy`] to fit [`MAX_ENCODED_TRANSACTION_SIZE`]
    /// Returns size of tx after optimizations
    fn optimize_strategy(
        tasks: &mut [Box<dyn BaseTask>],
    ) -> Result<usize, TaskStrategistError> {
        // Get initial transaction size
        let calculate_tx_length = |tasks: &[Box<dyn BaseTask>]| {
            match TransactionUtils::assemble_tasks_tx(
                &Keypair::new(), // placeholder
                tasks,
                u64::default(), // placeholder
                &[],
            ) {
                Ok(tx) => Ok(serialize_and_encode_base64(&tx).len()),
                Err(TaskStrategistError::FailedToFitError) => Ok(usize::MAX),
                Err(err) => Err(err),
            }
        };

        // Get initial transaction size
        let mut current_tx_length = calculate_tx_length(tasks)?;
        if current_tx_length <= MAX_ENCODED_TRANSACTION_SIZE {
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
            if current_tx_length <= MAX_ENCODED_TRANSACTION_SIZE {
                break;
            }

            let task = {
                // This is tmp task that will be replaced by old or optimized one
                let tmp_task =
                    ArgsTask::new(ArgsTaskType::Finalize(FinalizeTask {
                        delegated_account: Pubkey::new_unique(),
                    }));
                let tmp_task = Box::new(tmp_task) as Box<dyn BaseTask>;
                std::mem::replace(&mut tasks[index], tmp_task)
            };
            match task.optimize() {
                // If we can decrease:
                // 1. Calculate new tx size & ix size
                // 2. Insert item's data back in the heap
                // 3. Update overall tx size
                Ok(optimized_task) => {
                    tasks[index] = optimized_task;
                    let new_ix =
                        tasks[index].instruction(&Pubkey::new_unique());
                    // Possible serialization failures are possible only due to size in our case
                    // In that case we set size to max
                    let new_ix_size =
                        bincode::serialized_size(&new_ix).unwrap_or(u64::MAX);
                    let new_ix_size =
                        usize::try_from(new_ix_size).unwrap_or(usize::MAX);
                    current_tx_length = calculate_tx_length(tasks)?;
                    map.push((new_ix_size, index));
                }
                // That means el-t can't be optimized further
                // We move it back with oldest state
                // Heap forgets about this el-t
                Err(old_task) => {
                    tasks[index] = old_task;
                }
            }
        }

        Ok(current_tx_length)
    }
}

#[derive(thiserror::Error, Debug, PartialEq)]
pub enum TaskStrategistError {
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    #[error("SignerError: {0}")]
    SignerError(#[from] SignerError),
    #[error("Inconsistent task compression")]
    InconsistentTaskCompression,
}

pub type TaskStrategistResult<T, E = TaskStrategistError> = Result<T, E>;

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use light_client::indexer::photon_indexer::PhotonIndexer;
    use magicblock_program::magic_scheduled_base_intent::{
        BaseAction, CommittedAccount, ProgramArgs,
    };
    use solana_account::Account;
    use solana_pubkey::Pubkey;
    use solana_sdk::system_program;

    use super::*;
    use crate::{
        intent_execution_manager::intent_scheduler::create_test_intent,
        intent_executor::task_info_fetcher::{
            ResetType, TaskInfoFetcher, TaskInfoFetcherResult,
        },
        persist::IntentPersisterImpl,
        tasks::{
            task_builder::{CompressedData, TaskBuilderImpl, TasksBuilder},
            BaseActionTask, CommitTask, CompressedCommitTask, TaskStrategy,
            UndelegateTask,
        },
    };

    struct MockInfoFetcher;
    #[async_trait::async_trait]
    impl TaskInfoFetcher for MockInfoFetcher {
        async fn fetch_next_commit_ids(
            &self,
            pubkeys: &[Pubkey],
            _compressed: bool,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            Ok(pubkeys.iter().map(|pubkey| (*pubkey, 0)).collect())
        }

        async fn fetch_rent_reimbursements(
            &self,
            pubkeys: &[Pubkey],
        ) -> TaskInfoFetcherResult<Vec<Pubkey>> {
            Ok(pubkeys.iter().map(|_| Pubkey::new_unique()).collect())
        }

        fn peek_commit_id(&self, _pubkey: &Pubkey) -> Option<u64> {
            Some(0)
        }

        fn reset(&self, _: ResetType) {}
    }

    // Helper to create a simple commit task
    fn create_test_commit_task(commit_id: u64, data_size: usize) -> ArgsTask {
        ArgsTask::new(ArgsTaskType::Commit(CommitTask {
            commit_id,
            allow_undelegation: false,
            committed_account: CommittedAccount {
                pubkey: Pubkey::new_unique(),
                account: Account {
                    lamports: 1000,
                    data: vec![1; data_size],
                    owner: system_program::id(),
                    executable: false,
                    rent_epoch: 0,
                },
            },
        }))
    }

    // Helper to create a simple compressed commit task
    fn create_test_compressed_commit_task(
        commit_id: u64,
        data_size: usize,
    ) -> ArgsTask {
        ArgsTask::new(ArgsTaskType::CompressedCommit(CompressedCommitTask {
            commit_id,
            allow_undelegation: false,
            committed_account: CommittedAccount {
                pubkey: Pubkey::new_unique(),
                account: Account {
                    lamports: 1000,
                    data: vec![1; data_size],
                    owner: system_program::id(),
                    executable: false,
                    rent_epoch: 0,
                },
            },
            compressed_data: CompressedData::default(),
        }))
    }

    // Helper to create a Base action task
    fn create_test_base_action_task(len: usize) -> ArgsTask {
        ArgsTask::new(ArgsTaskType::BaseAction(BaseActionTask {
            action: BaseAction {
                destination_program: Pubkey::new_unique(),
                escrow_authority: Pubkey::new_unique(),
                account_metas_per_program: vec![],
                data_per_program: ProgramArgs {
                    data: vec![0; len],
                    escrow_index: 0,
                },
                compute_units: 30_000,
            },
        }))
    }

    // Helper to create a finalize task
    fn create_test_finalize_task() -> ArgsTask {
        ArgsTask::new(ArgsTaskType::Finalize(FinalizeTask {
            delegated_account: Pubkey::new_unique(),
        }))
    }

    // Helper to create an undelegate task
    fn create_test_undelegate_task() -> ArgsTask {
        ArgsTask::new(ArgsTaskType::Undelegate(UndelegateTask {
            delegated_account: Pubkey::new_unique(),
            owner_program: system_program::id(),
            rent_reimbursement: Pubkey::new_unique(),
        }))
    }

    #[test]
    fn test_build_strategy_with_single_small_task() {
        let validator = Pubkey::new_unique();
        let task = create_test_commit_task(1, 100);
        let tasks = vec![Box::new(task) as Box<dyn BaseTask>];

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
        )
        .expect("Should build strategy");

        assert_eq!(strategy.optimized_tasks.len(), 1);
        assert!(strategy.lookup_tables_keys.is_empty());
    }

    #[test]
    fn test_build_strategy_optimizes_to_buffer_when_needed() {
        let validator = Pubkey::new_unique();

        let task = create_test_commit_task(1, 1000); // Large task
        let tasks = vec![Box::new(task) as Box<dyn BaseTask>];

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
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

        let task = create_test_commit_task(1, 66_000); // Large task
        let tasks = vec![Box::new(task) as Box<dyn BaseTask>];

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
        )
        .expect("Should build strategy with buffer optimization");

        assert_eq!(strategy.optimized_tasks.len(), 1);
        assert!(matches!(
            strategy.optimized_tasks[0].strategy(),
            TaskStrategy::Buffer
        ));
    }

    #[test]
    fn test_build_strategy_creates_multiple_buffers() {
        // TODO: ALSO MAX NUM WITH PURE BUFFER commits, no alts
        const NUM_COMMITS: u64 = 3;

        let validator = Pubkey::new_unique();

        let tasks = (0..NUM_COMMITS)
            .map(|i| {
                let task = create_test_commit_task(i, 500); // Large task
                Box::new(task) as Box<dyn BaseTask>
            })
            .collect();

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
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
        const NUM_COMMITS: u64 = 22;

        let validator = Pubkey::new_unique();

        let tasks = (0..NUM_COMMITS)
            .map(|i| {
                // Large task
                let task = create_test_commit_task(i, 10000);
                Box::new(task) as Box<dyn BaseTask>
            })
            .collect();

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
        )
        .expect("Should build strategy with buffer optimization");

        for optimized_task in strategy.optimized_tasks {
            assert!(matches!(optimized_task.strategy(), TaskStrategy::Buffer));
        }
        assert!(!strategy.lookup_tables_keys.is_empty());
    }

    #[test]
    fn test_build_strategy_fails_when_cant_fit() {
        const NUM_COMMITS: u64 = 23;

        let validator = Pubkey::new_unique();

        let tasks = (0..NUM_COMMITS)
            .map(|i| {
                // Large task
                let task = create_test_commit_task(i, 1000);
                Box::new(task) as Box<dyn BaseTask>
            })
            .collect();

        let result = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
        );
        assert!(matches!(result, Err(TaskStrategistError::FailedToFitError)));
    }

    #[test]
    fn test_optimize_strategy_prioritizes_largest_tasks() {
        let mut tasks = [
            Box::new(create_test_commit_task(1, 100)) as Box<dyn BaseTask>,
            Box::new(create_test_commit_task(2, 1000)) as Box<dyn BaseTask>, // Larger task
            Box::new(create_test_commit_task(3, 1000)) as Box<dyn BaseTask>, // Larger task
        ];

        let _ = TaskStrategist::optimize_strategy(&mut tasks);
        // The larger task should have been optimized first
        assert!(matches!(tasks[0].strategy(), TaskStrategy::Args));
        assert!(matches!(tasks[1].strategy(), TaskStrategy::Buffer));
    }

    #[test]
    fn test_mixed_task_types_with_optimization() {
        let validator = Pubkey::new_unique();
        let tasks = vec![
            Box::new(create_test_commit_task(1, 1000)) as Box<dyn BaseTask>,
            Box::new(create_test_finalize_task()) as Box<dyn BaseTask>,
            Box::new(create_test_base_action_task(500)) as Box<dyn BaseTask>,
            Box::new(create_test_undelegate_task()) as Box<dyn BaseTask>,
        ];

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
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
        // This means that couldn't squeeze task optimization
        // So had to switch to ALTs
        // As expected
        assert!(!strategy.lookup_tables_keys.is_empty());
    }

    #[test]
    fn test_mixed_task_types_compressed() {
        let validator = Pubkey::new_unique();
        let tasks = vec![
            Box::new(create_test_commit_task(1, 100)) as Box<dyn BaseTask>,
            Box::new(create_test_compressed_commit_task(2, 100))
                as Box<dyn BaseTask>,
        ];

        let Err(err) = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<IntentPersisterImpl>,
        ) else {
            panic!("Should not build invalid strategy");
        };

        assert_eq!(err, TaskStrategistError::InconsistentTaskCompression);
    }

    #[tokio::test]
    async fn test_build_single_stage_mode() {
        let pubkey = [Pubkey::new_unique()];
        let intent = create_test_intent(0, &pubkey, false);

        let info_fetcher = Arc::new(MockInfoFetcher);
        let commit_task = TaskBuilderImpl::commit_tasks(
            &info_fetcher,
            &intent,
            &None::<IntentPersisterImpl>,
            &None::<Arc<PhotonIndexer>>,
        )
        .await
        .unwrap();
        let finalize_task = TaskBuilderImpl::finalize_tasks(
            &info_fetcher,
            &intent,
            &None::<Arc<PhotonIndexer>>,
        )
        .await
        .unwrap();

        let execution_mode = TaskStrategist::build_execution_strategy(
            commit_task,
            finalize_task,
            &Pubkey::new_unique(),
            &None::<IntentPersisterImpl>,
        )
        .expect("Execution mode created");

        let StrategyExecutionMode::SingleStage(value) = execution_mode else {
            panic!("Unexpected execution mode");
        };
        assert!(!value.uses_alts());
    }

    #[tokio::test]
    async fn test_build_two_stage_mode_no_alts() {
        let pubkeys: [_; 3] = std::array::from_fn(|_| Pubkey::new_unique());
        let intent = create_test_intent(0, &pubkeys, true);

        let info_fetcher = Arc::new(MockInfoFetcher);
        let commit_task = TaskBuilderImpl::commit_tasks(
            &info_fetcher,
            &intent,
            &None::<IntentPersisterImpl>,
            &None::<Arc<PhotonIndexer>>,
        )
        .await
        .unwrap();
        let finalize_task = TaskBuilderImpl::finalize_tasks(
            &info_fetcher,
            &intent,
            &None::<Arc<PhotonIndexer>>,
        )
        .await
        .unwrap();

        let execution_mode = TaskStrategist::build_execution_strategy(
            commit_task,
            finalize_task,
            &Pubkey::new_unique(),
            &None::<IntentPersisterImpl>,
        )
        .expect("Execution mode created");

        let StrategyExecutionMode::TwoStage {
            commit_stage,
            finalize_stage,
        } = execution_mode
        else {
            panic!("Unexpected execution mode");
        };
        assert!(!commit_stage.uses_alts());
        assert!(!finalize_stage.uses_alts());
    }

    #[tokio::test]
    async fn test_build_single_stage_mode_with_alts() {
        let pubkeys: [_; 8] = std::array::from_fn(|_| Pubkey::new_unique());
        let intent = create_test_intent(0, &pubkeys, false);

        let info_fetcher = Arc::new(MockInfoFetcher);
        let commit_task = TaskBuilderImpl::commit_tasks(
            &info_fetcher,
            &intent,
            &None::<IntentPersisterImpl>,
            &None::<Arc<PhotonIndexer>>,
        )
        .await
        .unwrap();
        let finalize_task = TaskBuilderImpl::finalize_tasks(
            &info_fetcher,
            &intent,
            &None::<Arc<PhotonIndexer>>,
        )
        .await
        .unwrap();

        let execution_mode = TaskStrategist::build_execution_strategy(
            commit_task,
            finalize_task,
            &Pubkey::new_unique(),
            &None::<IntentPersisterImpl>,
        )
        .expect("Execution mode created");

        let StrategyExecutionMode::SingleStage(value) = execution_mode else {
            panic!("Unexpected execution mode");
        };
        assert!(value.uses_alts());
    }
}
