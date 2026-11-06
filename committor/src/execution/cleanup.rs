use std::collections::HashSet;

use futures_util::future::join_all;
use solana_keypair::Keypair;

use crate::execution::preparation::{
    TransactionPreparator, cleanup_resources::CleanupResources,
    delivery_preparator::BufferExecutionError,
};

pub(super) async fn clean(
    authority: &Keypair,
    transaction_preparator: &dyn TransactionPreparator,
    mut resources: Vec<CleanupResources>,
    close_buffers: bool,
) -> Result<(), BufferExecutionError> {
    // Retries can prepare the same buffer again. Close it once, but release
    // every ALT reservation, even when separate attempts used identical keys.
    let mut buffers = HashSet::new();
    for resources in &mut resources {
        resources.buffers.retain(|task| buffers.insert((task.pubkey, task.commit_id)));
    }
    let cleanup_futs = resources.iter().map(|to_cleanup| {
        transaction_preparator.cleanup_resources(authority, to_cleanup, close_buffers)
    });

    join_all(cleanup_futs)
        .await
        .into_iter()
        .find_map(Result::err)
        .map_or(Ok(()), Err)
}
