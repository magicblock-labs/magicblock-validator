#![allow(unused)]
use lazy_static::lazy_static;
use solana_sdk::{clock::Slot, pubkey::Pubkey};
use std::sync::{Arc, RwLock};

pub struct ScheduledCommit {
    pub id: u64,
    pub slot: Slot,
    pub accounts: Vec<Pubkey>,
}

#[derive(Clone)]
pub struct TransactionScheduler {
    scheduled_commits: Arc<RwLock<Vec<ScheduledCommit>>>,
}

impl Default for TransactionScheduler {
    fn default() -> Self {
        lazy_static! {
            static ref SCHEDULED_COMMITS: Arc<RwLock<Vec<ScheduledCommit>>> =
                Default::default();
        }
        Self {
            scheduled_commits: SCHEDULED_COMMITS.clone(),
        }
    }
}

impl TransactionScheduler {
    /// Use in tests in order to avoid global state.
    /// Everywhere else use [TransactionScheduler::default()] which will share
    /// the scheduled transactions.
    pub fn new(scheduled_commits: Arc<RwLock<Vec<ScheduledCommit>>>) -> Self {
        Self { scheduled_commits }
    }

    pub fn schedule_commit(&self, commit: ScheduledCommit) {
        self.scheduled_commits
            .write()
            .expect("scheduled_commits lock poisoned")
            .push(commit);
    }
}
