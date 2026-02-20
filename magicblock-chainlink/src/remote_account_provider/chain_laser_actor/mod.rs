use std::{collections::HashSet, pin::Pin, sync::Arc};

use async_trait::async_trait;
use futures_util::Stream;
use helius_laserstream::{
    grpc::{SubscribeRequest, SubscribeUpdate},
    LaserstreamError, StreamHandle as HeliusStreamHandle,
};
use parking_lot::RwLock;
use solana_pubkey::Pubkey;

pub use self::{
    actor::{ChainLaserActor, Slots},
    stream_manager::{StreamManager, StreamManagerConfig, StreamUpdateSource},
};
use crate::remote_account_provider::{
    RemoteAccountProviderError, RemoteAccountProviderResult,
};

pub type SharedSubscriptions = Arc<RwLock<HashSet<Pubkey>>>;

mod actor;
#[cfg(test)]
mod mock;
mod stream_manager;

/// Result of a laser stream operation
pub type LaserResult = Result<SubscribeUpdate, LaserstreamError>;

/// A laser stream of subscription updates
pub type LaserStream = Pin<Box<dyn Stream<Item = LaserResult> + Send>>;

/// Abstraction over stream creation for testability
#[async_trait]
pub trait StreamFactory<S: StreamHandle>: Send + Sync + 'static {
    /// Create a stream for the given subscription request.
    async fn subscribe(
        &self,
        request: SubscribeRequest,
    ) -> RemoteAccountProviderResult<LaserStreamWithHandle<S>>;
}

/// A trait to represent the [HeliusStreamHandle].
/// This is needed since we cannot create the helius one since
/// [helius_laserstream::StreamHandle::write_tx] is private and there is no constructor.
#[async_trait]
pub trait StreamHandle {
    /// Send a new subscription request to update the active subscription.
    async fn write(
        &self,
        request: SubscribeRequest,
    ) -> Result<(), LaserstreamError>;
}

pub struct LaserStreamWithHandle<S: StreamHandle> {
    pub(crate) stream: LaserStream,
    pub(crate) handle: S,
}

pub struct StreamHandleImpl {
    pub handle: HeliusStreamHandle,
}

#[async_trait]
impl StreamHandle for StreamHandleImpl {
    async fn write(
        &self,
        request: SubscribeRequest,
    ) -> Result<(), LaserstreamError> {
        // This async operation gets forwarded to the underlying subscription sender of the laser
        // client and completes after the given item has been fully processed into the sink,
        // including flushing.
        // The assumption is that at that point it has been processed on the receiver end and the
        // subscription is updated.
        // See: https://github.com/helius-labs/laserstream-sdk/blob/v0.2.2/rust/src/client.rs#L196-L201
        self.handle.write(request).await
    }
}

/// Production stream factory that wraps helius client subscribe
pub struct StreamFactoryImpl {
    config: helius_laserstream::LaserstreamConfig,
}

impl StreamFactoryImpl {
    pub fn new(config: helius_laserstream::LaserstreamConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl StreamFactory<StreamHandleImpl> for StreamFactoryImpl {
    /// This implementation creates the underlying gRPC stream with the request (which
    /// returns immediately) and then writes the same request to the handle so it is sent
    /// over the network before returning asynchronously.
    async fn subscribe(
        &self,
        request: SubscribeRequest,
    ) -> RemoteAccountProviderResult<LaserStreamWithHandle<StreamHandleImpl>>
    {
        // NOTE: this call returns immediately yielding subscription errors and account updates
        // via the stream, thus the subscription has not been received yet upstream
        // NOTE: we need to use the same request as otherwise there is a potential race condition
        // where our `write` below completes before this call and the request is overwritten
        // with an empty one.
        // Inside helius_laserstream::client::subscribe the request is cloned for that reason to
        // be able to attempt the request multiple times during reconnect attempts.
        // Given our requests contain a max of about 2_000 pubkeys, an extra clone here is a small
        // price to pay to avoid this race condition.
        let (stream, handle) = helius_laserstream::client::subscribe(
            self.config.clone(),
            request.clone(),
        );
        let handle = StreamHandleImpl { handle };
        // Write to the handle and await it which at least guarantees that it has
        // been sent over the network, even though there is still no guarantee it has been
        // processed and that the subscription became active immediately
        handle.write(request).await.map_err(|err| {
            RemoteAccountProviderError::GrpcSubscriptionUpdateFailed(
                "subscribe".to_string(),
                0,
                format!("{err} ({err:?})"),
            )
        })?;
        Ok(LaserStreamWithHandle {
            stream: Box::pin(stream),
            handle,
        })
    }
}
