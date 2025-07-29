use std::{collections::BinaryHeap, ptr::NonNull};

use solana_pubkey::Pubkey;
use solana_sdk::{signature::Keypair, signer::Signer};

use crate::{
    persist::L1MessagesPersisterIface,
    tasks::{
        task_visitors::persistor_visitor::{
            PersistorContext, PersistorVisitor,
        },
        tasks::{ArgsTask, FinalizeTask, L1Task},
        utils::TransactionUtils,
    },
    transactions::{serialize_and_encode_base64, MAX_ENCODED_TRANSACTION_SIZE},
};

pub struct TransactionStrategy {
    pub optimized_tasks: Vec<Box<dyn L1Task>>,
    pub lookup_tables_keys: Vec<Pubkey>,
}

pub struct TaskStrategist;
impl TaskStrategist {
    /// Returns [`TaskDeliveryStrategy`] for every [`Task`]
    /// Returns Error if all optimizations weren't enough
    pub fn build_strategy<P: L1MessagesPersisterIface>(
        mut tasks: Vec<Box<dyn L1Task>>,
        validator: &Pubkey,
        persistor: &Option<P>,
    ) -> TaskStrategistResult<TransactionStrategy> {
        // Attempt optimizing tasks themselves(using buffers)
        if Self::optimize_strategy(&mut tasks) <= MAX_ENCODED_TRANSACTION_SIZE {
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
                Self::collect_lookup_table_keys(&validator, &tasks);
            Ok(TransactionStrategy {
                optimized_tasks: tasks,
                lookup_tables_keys,
            })
        } else {
            Err(Error::FailedToFitError)
        }
    }

    /// Attempt to use ALTs for ALL keys in tx
    /// Returns `true` if ALTs make tx fit, otherwise `false`
    /// TODO: optimize to use only necessary amount of pubkeys
    pub fn attempt_lookup_tables(tasks: &[Box<dyn L1Task>]) -> bool {
        let placeholder = Keypair::new();
        // Gather all involved keys in tx
        let budgets = TransactionUtils::tasks_compute_units(&tasks);
        let budget_instructions =
            TransactionUtils::budget_instructions(budgets, u64::default());
        let unique_involved_pubkeys = TransactionUtils::unique_involved_pubkeys(
            &tasks,
            &placeholder.pubkey(),
            &budget_instructions,
        );
        let dummy_lookup_tables =
            TransactionUtils::dummy_lookup_table(&unique_involved_pubkeys);

        // Create final tx
        let instructions =
            TransactionUtils::tasks_instructions(&placeholder.pubkey(), &tasks);
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
        if encoded_alt_tx.len() <= MAX_ENCODED_TRANSACTION_SIZE {
            true
        } else {
            false
        }
    }

    pub fn collect_lookup_table_keys(
        authority: &Pubkey,
        tasks: &[Box<dyn L1Task>],
    ) -> Vec<Pubkey> {
        let budgets = TransactionUtils::tasks_compute_units(&tasks);
        let budget_instructions =
            TransactionUtils::budget_instructions(budgets, u64::default());
        let unique_involved_pubkeys = TransactionUtils::unique_involved_pubkeys(
            &tasks,
            authority,
            &budget_instructions,
        );

        unique_involved_pubkeys
    }

    /// Optimizes set of [`TaskDeliveryStrategy`] to fit [`MAX_ENCODED_TRANSACTION_SIZE`]
    /// Returns size of tx after optimizations
    fn optimize_strategy(tasks: &mut [Box<dyn L1Task>]) -> usize {
        // Get initial transaction size
        let calculate_tx_length = |tasks: &[Box<dyn L1Task>]| {
            match TransactionUtils::assemble_tasks_tx(
                &Keypair::new(), // placeholder
                &tasks,
                u64::default(), // placeholder
                &[],
            ) {
                Ok(tx) => serialize_and_encode_base64(&tx).len(),
                Err(_) => usize::MAX,
            }
        };

        // Get initial transaction size
        let mut current_tx_length = calculate_tx_length(tasks);
        if current_tx_length <= MAX_ENCODED_TRANSACTION_SIZE {
            return current_tx_length;
        }

        // Create heap size -> index
        // TODO(edwin): OPTIMIZATION. update ixs arr, since we know index, coul then reuse for tx creation
        let ixs =
            TransactionUtils::tasks_instructions(&Pubkey::new_unique(), &tasks);
        let sizes = ixs
            .iter()
            .map(|ix| bincode::serialized_size(ix).map(|size| size as usize))
            .collect::<Result<Vec<usize>, _>>()
            .unwrap();
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
                let tmp_task = Box::new(tmp_task) as Box<dyn L1Task>;
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
                    let new_ix_size = bincode::serialized_size(&new_ix)
                        .expect("instruction serialization")
                        as usize;

                    current_tx_length = calculate_tx_length(tasks);
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

        current_tx_length
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Failed to fit in single TX")]
    FailedToFitError,
}

pub type TaskStrategistResult<T, E = Error> = Result<T, E>;

#[cfg(test)]
mod tests {
    use dlp::args::Context;
    use magicblock_program::magic_scheduled_l1_message::{
        CommittedAccountV2, L1Action, ProgramArgs,
    };
    use solana_account::Account;
    use solana_sdk::{signature::Keypair, system_program};

    use super::*;
    use crate::{
        persist::L1MessagePersister,
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
            action: L1Action {
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
        let tasks = vec![Box::new(task) as Box<dyn L1Task>];

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<L1MessagePersister>,
        )
        .expect("Should build strategy");

        assert_eq!(strategy.optimized_tasks.len(), 1);
        assert!(strategy.lookup_tables_keys.is_empty());
    }

    #[test]
    fn test_build_strategy_optimizes_to_buffer_when_needed() {
        let validator = Pubkey::new_unique();

        let task = create_test_commit_task(1, 1000); // Large task
        let tasks = vec![Box::new(task) as Box<dyn L1Task>];

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<L1MessagePersister>,
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
                Box::new(task) as Box<dyn L1Task>
            })
            .collect();

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<L1MessagePersister>,
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
                Box::new(task) as Box<dyn L1Task>
            })
            .collect();

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<L1MessagePersister>,
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
                Box::new(task) as Box<dyn L1Task>
            })
            .collect();

        let result = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<L1MessagePersister>,
        );
        assert!(matches!(result, Err(Error::FailedToFitError)));
    }

    #[test]
    fn test_optimize_strategy_prioritizes_largest_tasks() {
        let validator = Pubkey::new_unique();
        let mut tasks = vec![
            Box::new(create_test_commit_task(1, 100)) as Box<dyn L1Task>,
            Box::new(create_test_commit_task(2, 1000)) as Box<dyn L1Task>, // Larger task
            Box::new(create_test_commit_task(3, 1000)) as Box<dyn L1Task>, // Larger task
        ];

        let final_size = TaskStrategist::optimize_strategy(&mut tasks);
        // The larger task should have been optimized first
        assert!(matches!(tasks[0].strategy(), TaskStrategy::Args));
        assert!(matches!(tasks[1].strategy(), TaskStrategy::Buffer));
    }

    #[test]
    fn test_mixed_task_types_with_optimization() {
        let validator = Pubkey::new_unique();
        let tasks = vec![
            Box::new(create_test_commit_task(1, 1000)) as Box<dyn L1Task>,
            Box::new(create_test_finalize_task()) as Box<dyn L1Task>,
            Box::new(create_test_l1_action_task(500)) as Box<dyn L1Task>,
            Box::new(create_test_undelegate_task()) as Box<dyn L1Task>,
        ];

        let strategy = TaskStrategist::build_strategy(
            tasks,
            &validator,
            &None::<L1MessagePersister>,
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
