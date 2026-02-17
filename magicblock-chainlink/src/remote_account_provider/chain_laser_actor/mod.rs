use std::pin::Pin;

use futures_util::Stream;
use helius_laserstream::{
    grpc::{SubscribeRequest, SubscribeUpdate},
    LaserstreamError,
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
pub trait StreamFactory: Send + Sync + 'static {
    /// Create a stream for the given subscription request
    fn subscribe(&self, request: SubscribeRequest) -> LaserStream;
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

impl StreamFactory for StreamFactoryImpl {
    fn subscribe(&self, request: SubscribeRequest) -> LaserStream {
        let (stream, _handle) =
            helius_laserstream::client::subscribe(self.config.clone(), request);
        Box::pin(stream)
    }
}
