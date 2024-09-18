use serde::{Deserialize, Serialize};
use solana_sdk::{
    clock::Slot, hash::Hash, pubkey::Pubkey, transaction::Transaction,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledCommit {
    pub id: u64,
    pub slot: Slot,
    pub blockhash: Hash,
    pub accounts: Vec<Pubkey>,
    pub payer: Pubkey,
    pub owner: Pubkey,
    pub commit_sent_transaction: Transaction,
    pub request_undelegation: bool,
}

#[derive(Serialize, Deserialize)]
pub struct MagicContext {
    scheduled_commits: Vec<ScheduledCommit>,
}

impl MagicContext {
    pub(crate) fn add_scheduled_commit(&mut self, commit: ScheduledCommit) {
        self.scheduled_commits.push(commit);
    }
}
