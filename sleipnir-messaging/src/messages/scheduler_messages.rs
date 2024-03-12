// NOTE: from core/src/banking_stage/scheduler_messages.rs  with forward related code removed
#![allow(dead_code)]
use std::fmt::Display;

use solana_sdk::{clock::Slot, transaction::SanitizedTransaction};

/// A unique identifier for a transaction batch.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct TransactionBatchId(u64);

impl TransactionBatchId {
    pub fn new(index: u64) -> Self {
        Self(index)
    }
}

impl Display for TransactionBatchId {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A unique identifier for a transaction.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct TransactionId(u64);

impl TransactionId {
    pub fn new(index: u64) -> Self {
        Self(index)
    }
}

impl Display for TransactionId {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Message: [Scheduler -> Worker]
/// Transactions to be consumed (i.e. executed, recorded, and committed)
#[derive(Debug)]
pub struct ConsumeWork {
    pub batch_id: TransactionBatchId,
    pub ids: Vec<TransactionId>,
    pub transactions: Vec<SanitizedTransaction>,
    pub max_age_slots: Vec<Slot>,
}

/// Message: [Worker -> Scheduler]
/// Processed transactions.
pub struct FinishedConsumeWork {
    pub work: ConsumeWork,
    pub retryable_indexes: Vec<usize>,
}
