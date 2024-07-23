#![allow(unused)]
use lazy_static::lazy_static;
use solana_sdk::{clock::Slot, hash::Hash, pubkey::Pubkey};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledCommit {
    pub id: u64,
    pub slot: Slot,
    pub blockhash: Hash,
    pub accounts: Vec<Pubkey>,
    pub payer: Pubkey,
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
    pub fn schedule_commit(&self, commit: ScheduledCommit) {
        self.scheduled_commits
            .write()
            .expect("scheduled_commits lock poisoned")
            .push(commit);
    }

    pub fn get_scheduled_commits_by_payer(
        &self,
        payer: &Pubkey,
    ) -> Vec<ScheduledCommit> {
        let commits = self
            .scheduled_commits
            .read()
            .expect("scheduled_commits lock poisoned");
        commits
            .iter()
            .filter(|x| x.payer.eq(payer))
            .cloned()
            .collect::<Vec<_>>()
    }
}
