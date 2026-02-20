use std::{collections::HashSet, pin::Pin, sync::Arc};

use futures_util::Stream;
use helius_laserstream::{
    grpc::{SubscribeRequest, SubscribeUpdate},
    LaserstreamError,
};
use parking_lot::RwLock;
use solana_pubkey::Pubkey;
use tokio::time::Duration;

pub use self::{
    actor::{ChainLaserActor, Slots},
    stream_factory::{
        LaserStreamWithHandle, StreamFactory, StreamFactoryImpl, StreamHandle,
        StreamHandleImpl,
    },
    stream_manager::{StreamManager, StreamManagerConfig, StreamUpdateSource},
};
use crate::remote_account_provider::{
    RemoteAccountProviderError, RemoteAccountProviderResult,
};

pub type SharedSubscriptions = Arc<RwLock<HashSet<Pubkey>>>;

mod actor;
#[cfg(test)]
mod mock;
mod stream_factory;
mod stream_manager;

/// Retry a `handle.write(request)` call with linear backoff.
///
/// Tries up to `MAX_RETRIES` (5) times with 50 ms × attempt
/// backoff. Returns the original error after all retries are
/// exhausted.
pub(crate) async fn write_with_retry<S: StreamHandle>(
    handle: &S,
    task: &str,
    request: SubscribeRequest,
) -> RemoteAccountProviderResult<()> {
    const MAX_RETRIES: usize = 5;
    let mut retries = MAX_RETRIES;
    let initial_retries = retries;

    loop {
        match handle.write(request.clone()).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                if retries > 0 {
                    retries -= 1;
                    let backoff_ms = 50u64 * (initial_retries - retries) as u64;
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    continue;
                }
                return Err(
                    RemoteAccountProviderError::GrpcSubscriptionUpdateFailed(
                        task.to_string(),
                        MAX_RETRIES,
                        format!("{err} ({err:?})"),
                    ),
                );
            }
        }
    }
}

/// Result of a laser stream operation
pub type LaserResult = Result<SubscribeUpdate, LaserstreamError>;

/// A laser stream of subscription updates
pub type LaserStream = Pin<Box<dyn Stream<Item = LaserResult> + Send>>;
