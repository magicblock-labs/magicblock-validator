use std::collections::BinaryHeap;

use solana_pubkey::Pubkey;
use solana_sdk::{
    hash::Hash,
    instruction::Instruction,
    message::{v0::Message, VersionedMessage},
    signature::Keypair,
    transaction::{Transaction, VersionedTransaction},
};

use crate::{
    transaction_preperator::{
        error::{Error, PreparatorResult},
        task_builder::Task,
        utils::estimate_lookup_tables_for_tx,
    },
    transactions::{serialize_and_encode_base64, MAX_ENCODED_TRANSACTION_SIZE},
};

#[derive(Clone)]
pub enum TaskDeliveryStrategy {
    Args(Task),
    Buffer(Task),
}

impl TaskDeliveryStrategy {
    pub fn instruction(&self) -> Instruction {
        todo!()
    }
}

#[derive(Clone)]
pub struct TransactionStrategy {
    pub task_strategies: Vec<TaskDeliveryStrategy>,
    pub use_lookup_table: bool,
}

impl TaskDeliveryStrategy {
    fn decrease(self) -> Result<TaskDeliveryStrategy, TaskDeliveryStrategy> {
        match self {
            Self::Args(task) => {
                if task.is_bufferable() {
                    Ok(Self::Buffer(task))
                } else {
                    // Can't decrease size for task
                    Err(Self::Args(task))
                }
            }
            val @ Self::Buffer(_) => Err(val), // No other shorter strategy
        }
    }
}

pub struct DeliveryStrategist;
impl DeliveryStrategist {
    /// Returns [`TaskDeliveryStrategy`] for every [`Task`]
    pub fn build_strategies(
        tasks: Vec<Task>,
    ) -> PreparatorResult<TransactionStrategy> {
        // TODO(edwin): we could have Vec<dyn Task>
        // In runtime "BufferedTask" could replace "ArgTask"
        let mut strategies = tasks
            .into_iter()
            .map(|el| TaskDeliveryStrategy::Args(el))
            .collect::<Vec<_>>();

        // Optimize stategy
        if Self::optimize_strategy(&mut strategies)
            <= MAX_ENCODED_TRANSACTION_SIZE
        {
            return Ok(TransactionStrategy {
                task_strategies: strategies,
                use_lookup_table: false,
            });
        }

        let alt_tx = Self::assemble_tx_with_lookup_table(&strategies);
        if alt_tx.len() <= MAX_ENCODED_TRANSACTION_SIZE {
            Ok(TransactionStrategy {
                task_strategies: strategies,
                use_lookup_table: false,
            })
        } else {
            Err(Error::FailedToFitError)
        }
    }

    /// Optimizes set of [`TaskDeliveryStrategy`] to fit [`MAX_ENCODED_TRANSACTION_SIZE`]
    /// Returns size of tx after optimizations
    fn optimize_strategy(strategies: &mut [TaskDeliveryStrategy]) -> usize {
        let ixs = Self::assemble_ixs(&strategies);
        let tx = Self::assemble_tx_with_budget(&strategies);
        let mut current_tx_length = serialize_and_encode_base64(&tx).len();

        // Create heap size -> index
        let sizes = ixs
            .iter()
            .map(|ix| borsh::object_length(ix))
            .collect::<Result<Vec<usize>, _>>()
            .unwrap();

        let mut map = sizes
            .into_iter()
            .enumerate()
            .map(|(index, size)| (size, index))
            .collect::<BinaryHeap<_, _>>();
        // We keep popping heaviest el-ts & try to optimize while heap is non-empty
        while let Some((_, index)) = map.pop() {
            if current_tx_length <= MAX_ENCODED_TRANSACTION_SIZE {
                break;
            }

            let task = &mut strategies[index];
            // SAFETY: we have exclusive access to [`strategies`].
            // We create bitwise copy, and then replace it with `self`.
            // No memory will be double-freed since we use `std::ptr::write`
            // No memory is leaked since we use don't allocate anything new
            // Critical invariants:
            // - decrease(self) shall never drop self
            // NOTE: don't want [`Task`] to implement `Default`
            unsafe {
                let task_ptr = task as *mut TaskDeliveryStrategy;
                let old_value = std::ptr::read(task_ptr);
                match old_value.decrease() {
                    // If we can decrease:
                    // 1. Calculate new tx size & ix size
                    // 2. Insert item's data back in the heap
                    // 3. Update overall tx size
                    Ok(next_strategy) => {
                        std::ptr::write(task_ptr, next_strategy);
                        // TODO(edwin): this is expensive
                        let new_ix = strategies[index].instruction();
                        let new_ix_size =
                            borsh::object_length(&new_ix).unwrap(); // TODO(edwin): unwrap
                        let new_tx = Self::assemble_tx_with_budget(&strategies);

                        map.push((new_ix_size, index));
                        current_tx_length =
                            serialize_and_encode_base64(&new_tx).len();
                    }
                    // That means el-t can't be optimized further
                    // We move it back with oldest state
                    // Heap forgets about this el-t
                    Err(old_strategy) => {
                        std::ptr::write(task_ptr, old_strategy);
                    }
                }
            }
        }

        current_tx_length
    }

    // TODO(edwin): improve
    fn assemble_tx_with_lookup_table(
        strategies: &[TaskDeliveryStrategy],
    ) -> VersionedTransaction {
        // In case we can't fit with optimal strategy - try ALT
        let tx = Self::assemble_tx_with_budget(&strategies);
        let alts = estimate_lookup_tables_for_tx(&tx);
        let ixs = Self::assemble_ixs_with_budget(&strategies);
        let message = Message::try_compile(
            &Pubkey::new_unique(),
            &ixs,
            &alts,
            Hash::new_unique(),
        )
        .unwrap(); // TODO(edwin): unwrap
        let tx = VersionedTransaction::try_new(
            VersionedMessage::V0(message),
            &&[Keypair::new()],
        )
        .unwrap();
        tx
    }

    fn assemble_ixs_with_budget(
        strategies: &[TaskDeliveryStrategy],
    ) -> Vec<Instruction> {
        todo!()
    }

    fn assemble_ixs(tasks: &[TaskDeliveryStrategy]) -> Vec<Instruction> {
        // Just given Strategy(Task) creates dummy ixs
        // Then assemls ixs into tx
        todo!()
    }

    fn assemble_tx_with_budget(
        tasks: &[TaskDeliveryStrategy],
    ) -> VersionedTransaction {
        todo!()
    }
}
