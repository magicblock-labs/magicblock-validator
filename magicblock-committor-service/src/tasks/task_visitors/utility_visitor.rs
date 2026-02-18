use solana_pubkey::Pubkey;

use crate::tasks::{
    args_task::{ArgsTask, ArgsTaskType},
    buffer_task::{BufferTask, BufferTaskType},
    visitor::Visitor,
    BaseTask, BaseTaskImpl, FinalizeTask,
};

pub struct CommitMeta {
    pub committed_pubkey: Pubkey,
    pub commit_id: u64,
    pub remote_slot: u64,
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
    pub fn commit_meta(task: &BaseTaskImpl) -> Option<CommitMeta> {
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

        match &task.task_type {
            ArgsTaskType::Commit(task) => {
                *commit_meta = Some(CommitMeta {
                    committed_pubkey: task.committed_account.pubkey,
                    commit_id: task.commit_id,
                    remote_slot: task.committed_account.remote_slot,
                })
            }
            ArgsTaskType::CommitDiff(task) => {
                *commit_meta = Some(CommitMeta {
                    committed_pubkey: task.committed_account.pubkey,
                    commit_id: task.commit_id,
                    remote_slot: task.committed_account.remote_slot,
                })
            }
            _ => *commit_meta = None,
        }
    }

    fn visit_buffer_task(&mut self, task: &BufferTask) {
        let Self::GetCommitMeta(commit_meta) = self;

        match &task.task_type {
            BufferTaskType::Commit(task) => {
                *commit_meta = Some(CommitMeta {
                    committed_pubkey: task.committed_account.pubkey,
                    commit_id: task.commit_id,
                    remote_slot: task.committed_account.remote_slot,
                })
            }
            BufferTaskType::CommitDiff(task) => {
                *commit_meta = Some(CommitMeta {
                    committed_pubkey: task.committed_account.pubkey,
                    commit_id: task.commit_id,
                    remote_slot: task.committed_account.remote_slot,
                })
            }
        }
    }
}
