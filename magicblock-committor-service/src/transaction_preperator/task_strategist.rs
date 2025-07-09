use std::{
    collections::{BinaryHeap, HashSet},
    ptr::NonNull,
};

use solana_pubkey::Pubkey;
use solana_sdk::{
    address_lookup_table::AddressLookupTableAccount,
    hash::Hash,
    instruction::Instruction,
    message::{v0::Message, VersionedMessage},
    signature::Keypair,
    transaction::VersionedTransaction,
};

use crate::{
    transaction_preperator::{
        budget_calculator::ComputeBudgetV1,
        error::{Error, PreparatorResult},
        tasks::{ArgsTask, L1Task},
    },
    transactions::{serialize_and_encode_base64, MAX_ENCODED_TRANSACTION_SIZE},
};

pub struct TransactionStrategy {
    pub optimized_tasks: Vec<Box<dyn L1Task>>,
    pub lookup_tables_keys: Vec<Vec<Pubkey>>,
}

pub struct TaskStrategist;
impl TaskStrategist {
    /// Returns [`TaskDeliveryStrategy`] for every [`Task`]
    pub fn build_strategy(
        mut tasks: Vec<Box<dyn L1Task>>,
        validator: &Pubkey,
    ) -> PreparatorResult<TransactionStrategy> {
        // Optimize srategy
        if Self::optimize_strategy(&mut tasks) <= MAX_ENCODED_TRANSACTION_SIZE {
            return Ok(TransactionStrategy {
                optimized_tasks: tasks,
                lookup_tables_keys: vec![],
            });
        }

        let budget_instructions = Self::budget_instructions(
            &tasks.iter().map(|task| task.budget()).collect::<Vec<_>>(),
        );
        let lookup_tables = Self::assemble_lookup_table(
            &mut tasks,
            validator,
            &budget_instructions,
        );
        let alt_tx = Self::assemble_tx_with_lookup_table(
            &tasks,
            &budget_instructions,
            &lookup_tables,
        );
        let encoded_alt_tx = serialize_and_encode_base64(&alt_tx);
        if encoded_alt_tx.len() <= MAX_ENCODED_TRANSACTION_SIZE {
            let lookup_tables_keys = lookup_tables
                .into_iter()
                .map(|table| table.addresses)
                .collect();
            Ok(TransactionStrategy {
                optimized_tasks: tasks,
                lookup_tables_keys,
            })
        } else {
            Err(Error::FailedToFitError)
        }
    }

    /// Optimizes set of [`TaskDeliveryStrategy`] to fit [`MAX_ENCODED_TRANSACTION_SIZE`]
    /// Returns size of tx after optimizations
    fn optimize_strategy(tasks: &mut [Box<dyn L1Task>]) -> usize {
        let ixs = Self::assemble_ixs(&tasks);
        let tx = Self::assemble_tx_with_budget(&tasks);
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
                    // TODO(edwin): this is expensive
                    let new_ix =
                        tasks[index].instruction(&Pubkey::new_unique());
                    let new_ix_size = borsh::object_length(&new_ix).unwrap(); // TODO(edwin): unwrap
                    let new_tx = Self::assemble_tx_with_budget(&tasks);

                    map.push((new_ix_size, index));
                    current_tx_length =
                        serialize_and_encode_base64(&new_tx).len();
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

    fn assemble_lookup_table(
        tasks: &[Box<dyn L1Task>],
        validator: &Pubkey,
        budget_instructions: &[Instruction],
    ) -> Vec<AddressLookupTableAccount> {
        // Collect all unique pubkeys from tasks and budget instructions
        let mut all_pubkeys: HashSet<Pubkey> = tasks
            .iter()
            .flat_map(|task| task.involved_accounts(validator))
            .collect();

        all_pubkeys.extend(
            budget_instructions
                .iter()
                .flat_map(|ix| ix.accounts.iter().map(|meta| meta.pubkey)),
        );

        // Split into chunks of max 256 addresses
        all_pubkeys
            .into_iter()
            .collect::<Vec<_>>()
            .chunks(256)
            .map(|addresses| AddressLookupTableAccount {
                key: Pubkey::new_unique(),
                addresses: addresses.to_vec(),
            })
            .collect()
    }

    // TODO(edwin): improve
    fn assemble_tx_with_lookup_table(
        tasks: &[Box<dyn L1Task>],
        budget_instructions: &[Instruction],
        lookup_tables: &[AddressLookupTableAccount],
    ) -> VersionedTransaction {
        // In case we can't fit with optimal strategy - try ALT
        let ixs = Self::assemble_ixs_with_budget(&tasks);
        let message = Message::try_compile(
            &Pubkey::new_unique(),
            &[budget_instructions, &ixs].concat(),
            &lookup_tables,
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

    fn budget_instructions(budgets: &[ComputeBudgetV1]) -> Vec<Instruction> {
        todo!()
    }

    fn assemble_ixs_with_budget(
        strategies: &[Box<dyn L1Task>],
    ) -> Vec<Instruction> {
        todo!()
    }

    fn assemble_ixs(tasks: &[Box<dyn L1Task>]) -> Vec<Instruction> {
        // Just given Strategy(Task) creates dummy ixs
        // Then assemls ixs into tx
        todo!()
    }

    fn assemble_tx_with_budget(
        tasks: &[Box<dyn L1Task>],
    ) -> VersionedTransaction {
        todo!()
    }
}
