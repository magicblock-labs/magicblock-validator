use solana_pubkey::Pubkey;

use crate::tasks::{
    args_task::{ArgsTask, ArgsTaskType},
    buffer_task::{BufferTask, BufferTaskType},
    visitor::Visitor,
};

pub struct CommitMeta {
    pub committed_pubkey: Pubkey,
    pub commit_id: u64,
}

pub enum TaskVisitorUtils {
    GetCommitMeta(Option<CommitMeta>),
}

impl Visitor for TaskVisitorUtils {
    fn visit_args_task(&mut self, task: &ArgsTask) {
        let Self::GetCommitMeta(commit_meta) = self;

        match &task.task_type {
            ArgsTaskType::Commit(task) => {
                *commit_meta = Some(CommitMeta {
                    committed_pubkey: task.committed_account.pubkey,
                    commit_id: task.commit_id,
                })
            }
            ArgsTaskType::CommitDiff(task) => {
                *commit_meta = Some(CommitMeta {
                    committed_pubkey: task.committed_account.pubkey,
                    commit_id: task.commit_id,
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
                })
            }
            BufferTaskType::CommitDiff(task) => {
                *commit_meta = Some(CommitMeta {
                    committed_pubkey: task.committed_account.pubkey,
                    commit_id: task.commit_id,
                })
            }
        }
    }
}
