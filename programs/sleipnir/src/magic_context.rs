use std::mem;

use serde::{Deserialize, Serialize};
use sleipnir_core::magic_program;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    clock::Slot,
    hash::Hash,
    pubkey::Pubkey,
    transaction::Transaction,
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

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MagicContext {
    pub scheduled_commits: Vec<ScheduledCommit>,
}

impl MagicContext {
    pub const SIZE: usize = magic_program::MAGIC_CONTEXT_SIZE;
    pub const ZERO: [u8; Self::SIZE] = [0; Self::SIZE];
    pub(crate) fn deserialize(
        data: &AccountSharedData,
    ) -> Result<Self, bincode::Error> {
        if data.data().is_empty() {
            Ok(Self::default())
        } else {
            data.deserialize_data()
        }
    }

    pub(crate) fn add_scheduled_commit(&mut self, commit: ScheduledCommit) {
        self.scheduled_commits.push(commit);
    }

    pub(crate) fn take_scheduled_commits(&mut self) -> Vec<ScheduledCommit> {
        mem::take(&mut self.scheduled_commits)
    }

    pub fn has_scheduled_commits(data: &[u8]) -> bool {
        // Currently we only store a vec of scheduduled commits in the MagicContext
        // The first 8 bytes contain the length of the vec
        // This works even if the length is actually stored as a u32
        // since we zero out the entire context whenever we update the vec
        match bincode::deserialize::<u64>(&data[0..8]) {
            Ok(len) => len != 0,
            Err(_) => false,
        }
    }
}
