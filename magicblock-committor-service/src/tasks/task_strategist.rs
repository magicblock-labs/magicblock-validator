use std::{collections::BinaryHeap, ptr::NonNull};

use solana_pubkey::Pubkey;
use solana_sdk::{signature::Keypair, signer::Signer};

use crate::{
    persist::L1MessagesPersisterIface,
    tasks::{
        task_visitors::persistor_visitor::{
            PersistorContext, PersistorVisitor,
        },
        tasks::{ArgsTask, L1Task},
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

            let task = &mut tasks[index];
            let task = {
                // SAFETY:
                // 1. We create a dangling pointer purely for temporary storage during replace
                // 2. The pointer is never dereferenced before being replaced
                // 3. No memory allocated, hence no leakage
                let dangling = NonNull::<ArgsTask>::dangling();
                let tmp_task = unsafe { Box::from_raw(dangling.as_ptr()) }
                    as Box<dyn L1Task>;

                std::mem::replace(task, tmp_task)
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
