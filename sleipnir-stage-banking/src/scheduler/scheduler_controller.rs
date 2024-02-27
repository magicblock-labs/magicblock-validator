// NOTE: from core/src/banking_stage/transaction_scheduler/scheduler_controller.rs
// with lots of pieces removed that we don't need
use std::sync::{Arc, RwLock};

use sleipnir_bank::bank::Bank;

use super::{
    prio_graph_scheduler::PrioGraphScheduler,
    transaction_state_container::TransactionStateContainer,
};

// Removed:
// - decision_maker: DecisionMaker,
// Commented out the parts that we still need to implement

/// Controls packet and transaction flow into scheduler, and scheduling execution.
pub(crate) struct SchedulerController {
    /// Packet/Transaction ingress.
    // packet_receiver: PacketDeserializer,

    // changed from BankForks since we only have one
    bank: Arc<RwLock<Bank>>,

    /// Generates unique IDs for incoming transactions.
    // transaction_id_generator: TransactionIdGenerator,
    /// Container for transaction state.
    /// Shared resource between `packet_receiver` and `scheduler`.
    container: TransactionStateContainer,

    /// State for scheduling and communicating with worker threads.
    scheduler: PrioGraphScheduler,
    /*
    /// Metrics tracking counts on transactions in different states.
    count_metrics: SchedulerCountMetrics,

    /// Metrics tracking time spent in different code sections.
    timing_metrics: SchedulerTimingMetrics,

    /// Metric report handles for the worker threads.
    worker_metrics: Vec<Arc<ConsumeWorkerMetrics>>,
    */
}
