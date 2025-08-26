use std::collections::BinaryHeap;

use solana_pubkey::Pubkey;
use solana_sdk::{
    signature::Keypair,
    signer::{Signer, SignerError},
};

use crate::{
    persist::IntentPersister,
    tasks::{
        task_visitors::persistor_visitor::{
            PersistorContext, PersistorVisitor,
        },
        tasks::{ArgsTask, BaseTask, FinalizeTask},
        utils::TransactionUtils,
    },
    transactions::{serialize_and_encode_base64, MAX_ENCODED_TRANSACTION_SIZE},
};

pub struct TransactionStrategy {
    pub optimized_tasks: Vec<Box<dyn BaseTask>>,
    pub lookup_tables_keys: Vec<Pubkey>,
}

pub struct TaskStrategist;
impl TaskStrategist {
    /// Returns [`TaskDeliveryStrategy`] for every [`Task`]
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

            Ok(TransactionStrategy {
                optimized_tasks: tasks,
                lookup_tables_keys: vec![],
            })
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
            Ok(TransactionStrategy {
                optimized_tasks: tasks,
                lookup_tables_keys,
            })
        } else {
            Err(TaskStrategistError::FailedToFitError)
        }
    }

    /// Attempt to use ALTs for ALL keys in tx
    /// Returns `true` if ALTs make tx fit, otherwise `false`
    /// TODO: optimize to use only necessary amount of pubkeys
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
    ) -> Result<usize, SignerError> {
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
                Err(TaskStrategistError::SignerError(err)) => Err(err),
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
                let tmp_task = ArgsTask::Finalize(FinalizeTask {
                    delegated_account: Pubkey::new_unique(),
                });
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

#[derive(thiserror::Error, Debug)]
pub enum TaskStrategistError {
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    #[error("SignerError: {0}")]
    SignerError(#[from] SignerError),
}

pub type TaskStrategistResult<T, E = TaskStrategistError> = Result<T, E>;

#[cfg(test)]
mod tests {
    use dlp::args::Context;
    use magicblock_program::magic_scheduled_base_intent::{
        BaseAction, CommittedAccountV2, ProgramArgs,
    };
    use solana_account::Account;
    use solana_sdk::system_program;

    use super::*;
    use crate::{
        persist::IntentPersisterImpl,
        tasks::tasks::{
            CommitTask, L1ActionTask, TaskStrategy, UndelegateTask,
        },
    };

    // Helper to create a simple commit task
    fn create_test_commit_task(commit_id: u64, data_size: usize) -> ArgsTask {
        ArgsTask::Commit(CommitTask {
            commit_id,
            allow_undelegation: false,
            committed_account: CommittedAccountV2 {
                pubkey: Pubkey::new_unique(),
                account: Account {
                    lamports: 1000,
                    data: vec![0; data_size],
                    owner: system_program::id(),
                    executable: false,
                    rent_epoch: 0,
                },
            },
        })
    }

    // Helper to create an L1 action task
    fn create_test_l1_action_task(len: usize) -> ArgsTask {
        ArgsTask::L1Action(L1ActionTask {
            context: Context::Commit,
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
        })
    }

    // Helper to create a finalize task
    fn create_test_finalize_task() -> ArgsTask {
        ArgsTask::Finalize(FinalizeTask {
            delegated_account: Pubkey::new_unique(),
        })
    }

    // Helper to create an undelegate task
    fn create_test_undelegate_task() -> ArgsTask {
        ArgsTask::Undelegate(UndelegateTask {
            delegated_account: Pubkey::new_unique(),
            owner_program: system_program::id(),
            rent_reimbursement: Pubkey::new_unique(),
        })
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
        // TODO: ALSO MAX NUMBER OF TASKS fit with ALTs!
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
            Box::new(create_test_l1_action_task(500)) as Box<dyn BaseTask>,
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
                TaskStrategy::Args,   // L1Action stays
                TaskStrategy::Args,   // Undelegate stays
            ]
        );
        // This means that couldn't squeeze task optimization
        // So had to switch to ALTs
        // As expected
        assert!(!strategy.lookup_tables_keys.is_empty());
    }
}
