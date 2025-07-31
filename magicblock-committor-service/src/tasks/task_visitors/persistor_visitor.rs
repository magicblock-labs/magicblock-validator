use log::error;

use crate::{
    persist::{CommitStrategy, IntentPersister},
    tasks::{
        tasks::{ArgsTask, BufferTask},
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

impl<'a, P> Visitor for PersistorVisitor<'a, P>
where
    P: IntentPersister,
{
    fn visit_args_task(&mut self, task: &ArgsTask) {
        match self.context {
            PersistorContext::PersistStrategy { uses_lookup_tables } => {
                let ArgsTask::Commit(commit_task) = task else {
                    return;
                };

                let commit_strategy = if uses_lookup_tables {
                    CommitStrategy::Args
                } else {
                    CommitStrategy::ArgsWithLookupTable
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
                let BufferTask::Commit(commit_task) = task;
                let commit_strategy = if uses_lookup_tables {
                    CommitStrategy::FromBuffer
                } else {
                    CommitStrategy::FromBufferWithLookupTable
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
