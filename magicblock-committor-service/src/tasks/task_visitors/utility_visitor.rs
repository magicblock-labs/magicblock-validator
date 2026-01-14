use solana_pubkey::Pubkey;

use crate::tasks::{
    args_task::{ArgsTask, ArgsTaskType},
    buffer_task::{BufferTask, BufferTaskType},
    visitor::Visitor,
    BaseTask, FinalizeTask,
};

pub struct CommitMeta {
    pub committed_pubkey: Pubkey,
    pub commit_id: u64,
}

impl From<CommitMeta> for FinalizeTask {
    fn from(value: CommitMeta) -> Self {
        FinalizeTask {
            delegated_account: value.committed_pubkey,
        }
    }
}

pub enum TaskVisitorUtils {
    GetCommitMeta(Option<CommitMeta>),
}

impl TaskVisitorUtils {
    pub fn commit_meta(task: &dyn BaseTask) -> Option<CommitMeta> {
        let mut v = TaskVisitorUtils::GetCommitMeta(None);
        task.visit(&mut v);

        match v {
            TaskVisitorUtils::GetCommitMeta(meta) => meta,
        }
    }
}

impl Visitor for TaskVisitorUtils {
    fn visit_args_task(&mut self, task: &ArgsTask) {
        let Self::GetCommitMeta(commit_meta) = self;

        if let ArgsTaskType::Commit(ref commit_task) = task.task_type {
            *commit_meta = Some(CommitMeta {
                committed_pubkey: commit_task.committed_account.pubkey,
                commit_id: commit_task.commit_id,
            })
        } else {
            *commit_meta = None
        }
    }

    fn visit_buffer_task(&mut self, task: &BufferTask) {
        let Self::GetCommitMeta(commit_meta) = self;

        let BufferTaskType::Commit(ref commit_task) = task.task_type;
        *commit_meta = Some(CommitMeta {
            committed_pubkey: commit_task.committed_account.pubkey,
            commit_id: commit_task.commit_id,
        })
    }
}
