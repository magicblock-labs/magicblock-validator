use log::error;

use crate::{
    persist::{CommitStrategy, IntentPersister},
    tasks::{visitor::Visitor, Task},
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
    fn visit_task(&mut self, task: &Task) {
        match self.context {
            PersistorContext::PersistStrategy { uses_lookup_tables } => {
                let Task::Commit(ref commit_task) = task else {
                    return;
                };

                let commit_strategy = if uses_lookup_tables {
                    CommitStrategy::StateArgsWithLookupTable
                } else {
                    CommitStrategy::StateArgs
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
