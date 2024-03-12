// NOTE: from core/src/banking_stage/transaction_scheduler/transaction_priority_id.rs
use std::hash::{Hash, Hasher};

use prio_graph::TopLevelId;
use sleipnir_messaging::scheduler_messages::TransactionId;

/// A unique identifier tied with priority ordering for a transaction/packet:
///     - `id` has no effect on ordering
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct TransactionPriorityId {
    pub(crate) priority: u64,
    pub(crate) id: TransactionId,
}

impl TransactionPriorityId {
    pub(crate) fn new(priority: u64, id: TransactionId) -> Self {
        Self { priority, id }
    }
}

impl Ord for TransactionPriorityId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.priority.cmp(&other.priority)
    }
}

impl PartialOrd for TransactionPriorityId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for TransactionPriorityId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state)
    }
}

impl TopLevelId<Self> for TransactionPriorityId {
    fn id(&self) -> Self {
        *self
    }
}
