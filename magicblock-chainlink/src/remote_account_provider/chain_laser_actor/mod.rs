use async_trait::async_trait;
use std::pin::Pin;

use futures_util::Stream;
use helius_laserstream::{
    LaserstreamError, StreamHandle as HeliusStreamHandle, grpc::{SubscribeRequest, SubscribeUpdate}
};

pub use self::{
    actor::{ChainLaserActor, SharedSubscriptions, Slots},
    stream_manager::{StreamManager, StreamManagerConfig},
};

mod actor;
mod mock;
mod stream_manager;

/// Result of a laser stream operation
pub type LaserResult = Result<SubscribeUpdate, LaserstreamError>;

/// A laser stream of subscription updates
pub type LaserStream = Pin<Box<dyn Stream<Item = LaserResult> + Send>>;

/// Abstraction over stream creation for testability
pub trait StreamFactory<S: StreamHandle>: Send + Sync + 'static {
    /// Create a stream for the given subscription request
    fn subscribe(&self, request: SubscribeRequest) -> LaserStreamWithHandle<S>;
}

/// A trait to represent the [HeliusStreamHandle].
/// This is needed since we cannot create the helius one since
/// [helius_laserstream::StreamHandle::write_tx] is private and there is no constructor.
#[async_trait]
pub trait StreamHandle {
    /// Send a new subscription request to update the active subscription.
    async fn write(&self, request: SubscribeRequest) -> Result<(), LaserstreamError>;
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
    async fn write(&self, request: SubscribeRequest) -> Result<(), LaserstreamError> {
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

impl StreamFactory<StreamHandleImpl> for StreamFactoryImpl {
    fn subscribe(&self, request: SubscribeRequest) -> LaserStreamWithHandle<StreamHandleImpl> {
        let (stream, handle) =
            helius_laserstream::client::subscribe(self.config.clone(), request);
        LaserStreamWithHandle {
            stream: Box::pin(stream),
            handle: StreamHandleImpl { handle },
        }
    }
}
