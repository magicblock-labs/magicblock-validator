// FIXME: once we worked this out
#![allow(dead_code)]
#![allow(unused_variables)]

use std::sync::{Arc, RwLock};

use solana_program_runtime::loaded_programs::{BlockRelation, ForkGraph, LoadedPrograms};
use solana_sdk::{
    clock::{Epoch, Slot},
    epoch_schedule::EpochSchedule,
    fee::FeeStructure,
};
use solana_svm::{runtime_config::RuntimeConfig, transaction_processor::TransactionBatchProcessor};

// -----------------
// ForkGraph
// -----------------
pub struct SimpleForkGraph;

impl ForkGraph for SimpleForkGraph {
    /// Returns the BlockRelation of A to B
    fn relationship(&self, a: Slot, b: Slot) -> BlockRelation {
        BlockRelation::Unrelated
    }

    /// Returns the epoch of the given slot
    fn slot_epoch(&self, _slot: Slot) -> Option<Epoch> {
        Some(0)
    }
}

// -----------------
// Bank
// -----------------
pub struct Bank {
    /// Bank slot (i.e. block)
    slot: Slot,

    /// Bank epoch
    epoch: Epoch,

    /// initialized from genesis
    pub(crate) epoch_schedule: EpochSchedule,

    /// Transaction fee structure
    pub fee_structure: FeeStructure,

    /// Optional config parameters that can override runtime behavior
    pub(crate) runtime_config: Arc<RuntimeConfig>,

    pub loaded_programs_cache: Arc<RwLock<LoadedPrograms<SimpleForkGraph>>>,

    transaction_processor: TransactionBatchProcessor<SimpleForkGraph>,
}

impl Default for Bank {
    fn default() -> Self {
        // NOTE: we plan to only have one bank which is created when the validator starts up
        let slot = 0;
        let epoch = 0;

        let epoch_schedule = EpochSchedule::default();
        let fee_structure = FeeStructure::default();
        let runtime_config = Arc::new(RuntimeConfig::default());
        let loaded_programs_cache = Arc::new(RwLock::new(LoadedPrograms::new(slot, epoch)));

        let transaction_processor = TransactionBatchProcessor::new(
            slot,
            epoch,
            epoch_schedule.clone(),
            fee_structure.clone(),
            runtime_config.clone(),
            loaded_programs_cache.clone(),
        );

        Self {
            slot,
            epoch,
            epoch_schedule,
            fee_structure,
            runtime_config,
            loaded_programs_cache,
            transaction_processor,
        }
    }
}
