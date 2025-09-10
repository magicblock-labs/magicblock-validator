use magicblock_committor_program::instruction_builder::close_buffer::{
    create_close_ix, CreateCloseIxArgs,
};
use solana_pubkey::Pubkey;
use solana_sdk::instruction::Instruction;

use crate::tasks::{
    visitor::Visitor, ArgsTask, ArgsTaskType, BufferTask, BufferTaskType,
};

pub struct CommitMeta {
    pub committed_pubkey: Pubkey,
    pub commit_id: u64,
}

pub enum TaskVisitorUtils {
    ChangeCommitId(u64),
    GetCommitMeta(Option<CommitMeta>),
}

impl Visitor for TaskVisitorUtils {
    fn visit_args_task_mut(&mut self, task: &mut ArgsTask) {
        match self {
            Self::ChangeCommitId(commit_id) => {
                if let ArgsTaskType::Commit(ref mut commit_task) =
                    task.task_type
                {
                    commit_task.commit_id = *commit_id
                };
            }
            _ => {}
        }
    }

    fn visit_buffer_task_mut(&mut self, task: &mut BufferTask) {
        match self {
            Self::ChangeCommitId(commit_id) => {
                let BufferTaskType::Commit(ref mut commit_task) =
                    task.task_type;
                commit_task.commit_id = *commit_id
            }
            _ => {}
        }
    }

    fn visit_buffer_task(&mut self, task: &BufferTask) {
        let Self::GetCommitMeta(commit_meta) = self else {
            return;
        };

        let BufferTaskType::Commit(ref commit_task) = task;
        *commit_meta = Some(CommitMeta {
            committed_pubkey: commit_task.committed_account.pubkey,
            commit_id: commit_task.commit_id,
        })
    }

    fn visit_args_task(&mut self, task: &ArgsTask) {
        let Self::GetCommitMeta(commit_meta) = self else {
            return;
        };

        if let ArgsTaskType::Commit(ref commit_task) = task.task_type {
            *commit_meta = Some(CommitMeta {
                committed_pubkey: commit_task.committed_account.pubkey,
                commit_id: commit_task.commit_id,
            })
        } else {
            *commit_meta = None
        }
    }
}
