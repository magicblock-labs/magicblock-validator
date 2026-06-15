use futures_util::future::try_join_all;
use solana_keypair::Keypair;

use crate::{
    tasks::task_strategist::TransactionStrategy,
    transaction_preparator::{
        delivery_preparator::BufferExecutionError, TransactionPreparator,
    },
};

pub struct CleanupHandle<T> {
    authority: Keypair,
    junk: Vec<TransactionStrategy>,
    /// When false (execution failed), only releases ALT reservations without
    /// closing buffer PDAs — avoids a race condition where a concurrent
    /// executor may still be using them.
    close_buffers: bool,
    transaction_preparator: T,
}

impl<T: TransactionPreparator> CleanupHandle<T> {
    pub(crate) fn new(
        authority: Keypair,
        junk: Vec<TransactionStrategy>,
        close_buffers: bool,
        transaction_preparator: T,
    ) -> Self {
        Self {
            junk,
            close_buffers,
            authority,
            transaction_preparator,
        }
    }

    pub(crate) async fn clean(self) -> Result<(), BufferExecutionError> {
        let close_buffers = self.close_buffers;
        let cleanup_futs = self.junk.iter().map(|to_cleanup| {
            self.transaction_preparator.cleanup_for_strategy(
                &self.authority,
                &to_cleanup.optimized_tasks,
                &to_cleanup.lookup_tables_keys,
                close_buffers,
            )
        });

        try_join_all(cleanup_futs).await.map(|_| ())
    }
}
