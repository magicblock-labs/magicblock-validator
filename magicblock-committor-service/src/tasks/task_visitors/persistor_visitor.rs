use log::error;

use crate::{
    persist::{CommitStrategy, IntentPersister},
    tasks::{visitor::Visitor, DataDelivery, Task},
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
        let PersistorContext::PersistStrategy { uses_lookup_tables } =
            self.context;

        match &task {
            Task::Commit(task) => {
                let commit_strategy = match task.data_delivery() {
                    DataDelivery::StateInArgs => {
                        if uses_lookup_tables {
                            CommitStrategy::StateArgsWithLookupTable
                        } else {
                            CommitStrategy::StateArgs
                        }
                    }
                    DataDelivery::StateInBuffer => {
                        if uses_lookup_tables {
                            CommitStrategy::StateBufferWithLookupTable
                        } else {
                            CommitStrategy::StateBuffer
                        }
                    }
                    DataDelivery::DiffInArgs => {
                        if uses_lookup_tables {
                            CommitStrategy::DiffArgsWithLookupTable
                        } else {
                            CommitStrategy::DiffArgs
                        }
                    }
                    DataDelivery::DiffInBuffer => {
                        if uses_lookup_tables {
                            CommitStrategy::DiffBufferWithLookupTable
                        } else {
                            CommitStrategy::DiffBuffer
                        }
                    }
                };

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
            Task::Finalize(_) | Task::Undelegate(_) | Task::BaseAction(_) => {}
        };
    }
}
