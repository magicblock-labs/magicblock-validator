use log::error;

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
                let commit_strategy = if uses_lookup_tables {
                    CommitStrategy::ArgsWithLookupTable
                } else {
                    CommitStrategy::Args
                };

                match &task.task_type {
                    ArgsTaskType::Commit(task) => {
                        if let Err(err) = self.persistor.set_commit_strategy(
                            task.commit_id,
                            &task.committed_account.pubkey,
                            commit_strategy,
                        ) {
                            error!(
                                "Failed to persist commit strategy {}: {}",
                                commit_strategy.as_str(),
                                err
                            );
                        }
                    }
                    ArgsTaskType::CommitDiff(task) => {
                        if let Err(err) = self.persistor.set_commit_strategy(
                            task.commit_id,
                            &task.committed_account.pubkey,
                            commit_strategy,
                        ) {
                            error!(
                                "Failed to persist commit strategy {}: {}",
                                commit_strategy.as_str(),
                                err
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn visit_buffer_task(&mut self, task: &BufferTask) {
        match self.context {
            PersistorContext::PersistStrategy { uses_lookup_tables } => {
                let commit_strategy = if uses_lookup_tables {
                    CommitStrategy::FromBufferWithLookupTable
                } else {
                    CommitStrategy::FromBuffer
                };

                match &task.task_type {
                    BufferTaskType::Commit(task) => {
                        if let Err(err) = self.persistor.set_commit_strategy(
                            task.commit_id,
                            &task.committed_account.pubkey,
                            commit_strategy,
                        ) {
                            error!(
                                "Failed to persist commit strategy {}: {}",
                                commit_strategy.as_str(),
                                err
                            );
                        }
                    }
                    BufferTaskType::CommitDiff(task) => {
                        if let Err(err) = self.persistor.set_commit_strategy(
                            task.commit_id,
                            &task.committed_account.pubkey,
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
    }
}
