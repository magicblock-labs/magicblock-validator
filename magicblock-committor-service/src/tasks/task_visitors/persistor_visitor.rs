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
                let ArgsTaskType::Commit(ref commit_task) = task.task_type
                else {
                    return;
                };

                let commit_strategy = if uses_lookup_tables {
                    CommitStrategy::ArgsWithLookupTable
                } else {
                    CommitStrategy::Args
                };

                if let Err(err) = self.persistor.set_commit_strategy(
                    commit_task.commit_id,
                    &commit_task.committed_account.pubkey,
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
                let BufferTaskType::Commit(ref commit_task) = task.task_type;
                let commit_strategy = if uses_lookup_tables {
                    CommitStrategy::FromBufferWithLookupTable
                } else {
                    CommitStrategy::FromBuffer
                };

                if let Err(err) = self.persistor.set_commit_strategy(
                    commit_task.commit_id,
                    &commit_task.committed_account.pubkey,
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
