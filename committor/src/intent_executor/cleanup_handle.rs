use std::collections::HashSet;

use futures_util::future::join_all;
use solana_keypair::Keypair;

use crate::transaction_preparator::{
    TransactionPreparator, cleanup_resources::CleanupResources,
    delivery_preparator::BufferExecutionError,
};

pub struct CleanupHandle<T> {
    authority: Keypair,
    resources: Vec<CleanupResources>,
    /// When false (execution failed), only releases ALT reservations without
    /// closing buffer PDAs — avoids a race condition where a concurrent
    /// executor may still be using them.
    close_buffers: bool,
    transaction_preparator: T,
}

impl<T: TransactionPreparator> CleanupHandle<T> {
    pub(crate) fn new(
        authority: Keypair,
        resources: Vec<CleanupResources>,
        close_buffers: bool,
        transaction_preparator: T,
    ) -> Self {
        Self {
            resources,
            close_buffers,
            authority,
            transaction_preparator,
        }
    }

    pub async fn clean(mut self) -> Result<(), BufferExecutionError> {
        let close_buffers = self.close_buffers;
        // Retries can prepare the same buffer again. Close it once, but release
        // every ALT reservation, even when separate attempts used identical keys.
        let mut buffers = HashSet::new();
        for resources in &mut self.resources {
            resources.buffers.retain(|task| buffers.insert((task.pubkey, task.commit_id)));
        }
        let cleanup_futs = self.resources.iter().map(|to_cleanup| {
            self.transaction_preparator.cleanup_resources(
                &self.authority,
                to_cleanup,
                close_buffers,
            )
        });

        join_all(cleanup_futs)
            .await
            .into_iter()
            .find_map(Result::err)
            .map_or(Ok(()), Err)
    }
}
