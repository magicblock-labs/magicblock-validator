use tracing::error;

use crate::{
    persist::{CommitStrategy, IntentPersister},
    tasks::{
        args_task::{ArgsTask, ArgsTaskType},
        buffer_task::{BufferTask, BufferTaskType},
        visitor::Visitor,
    },
};

pub enum PersistorContext {
    PersistStrategy { uses_lookup_tables: bool },
    // Other possible persist
}

pub struct PersistorVisitor<'a, P> {
    pub persistor: &'a P,
    pub context: PersistorContext,
}

impl<P> Visitor for PersistorVisitor<'_, P>
where
    P: IntentPersister,
{
    fn visit_args_task(&mut self, task: &ArgsTask) {
        match self.context {
            PersistorContext::PersistStrategy { uses_lookup_tables } => {
                let commit_strategy = |is_diff: bool| {
                    if is_diff {
                        if uses_lookup_tables {
                            CommitStrategy::DiffArgsWithLookupTable
                        } else {
                            CommitStrategy::DiffArgs
                        }
                    } else if uses_lookup_tables {
                        CommitStrategy::StateArgsWithLookupTable
                    } else {
                        CommitStrategy::StateArgs
                    }
                };

                let (commit_id, pubkey, commit_strategy) = match &task.task_type
                {
                    ArgsTaskType::Commit(task) => (
                        task.commit_id,
                        &task.committed_account.pubkey,
                        commit_strategy(false),
                    ),
                    ArgsTaskType::CommitDiff(task) => (
                        task.commit_id,
                        &task.committed_account.pubkey,
                        commit_strategy(true),
                    ),
                    _ => return,
                };

                if let Err(err) = self.persistor.set_commit_strategy(
                    commit_id,
                    pubkey,
                    commit_strategy,
                ) {
                    error!(
                        "Failed to persist commit strategy {}: {}",
                        commit_strategy.as_str(),
                        err
                    );
                }
            }
        }
    }

    fn visit_buffer_task(&mut self, task: &BufferTask) {
        match self.context {
            PersistorContext::PersistStrategy { uses_lookup_tables } => {
                let commit_strategy = |is_diff: bool| {
                    if is_diff {
                        if uses_lookup_tables {
                            CommitStrategy::DiffBufferWithLookupTable
                        } else {
                            CommitStrategy::DiffBuffer
                        }
                    } else if uses_lookup_tables {
                        CommitStrategy::StateBufferWithLookupTable
                    } else {
                        CommitStrategy::StateBuffer
                    }
                };

                let (commit_id, pubkey, commit_strategy) = match &task.task_type
                {
                    BufferTaskType::Commit(task) => (
                        task.commit_id,
                        &task.committed_account.pubkey,
                        commit_strategy(false),
                    ),
                    BufferTaskType::CommitDiff(task) => (
                        task.commit_id,
                        &task.committed_account.pubkey,
                        commit_strategy(true),
                    ),
                };

                if let Err(err) = self.persistor.set_commit_strategy(
                    commit_id,
                    pubkey,
                    commit_strategy,
                ) {
                    error!(
                        "Failed to persist commit strategy {}: {}",
                        commit_strategy.as_str(),
                        err
                    );
                }
            }
        }
    }
}
