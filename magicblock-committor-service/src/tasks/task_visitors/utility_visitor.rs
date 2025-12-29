use solana_pubkey::Pubkey;

use crate::tasks::{visitor::Visitor, Task};

pub struct CommitMeta {
    pub committed_pubkey: Pubkey,
    pub commit_id: u64,
}

pub enum TaskVisitorUtils {
    GetCommitMeta(Option<CommitMeta>),
}

impl Visitor for TaskVisitorUtils {
    fn visit_task(&mut self, task: &Task) {
        let Self::GetCommitMeta(commit_meta) = self;

        if let Task::Commit(ref commit_task) = task {
            *commit_meta = Some(CommitMeta {
                committed_pubkey: commit_task.committed_account.pubkey,

                commit_id: commit_task.commit_id,
            })
        } else {
            *commit_meta = None
        }
    }
}
