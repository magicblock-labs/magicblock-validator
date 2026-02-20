use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use helius_laserstream::{grpc::SubscribeRequest, LaserstreamError};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use super::{LaserResult, StreamFactory};
use crate::remote_account_provider::{
    chain_laser_actor::{LaserStreamWithHandle, StreamHandle},
    RemoteAccountProviderResult,
};

/// A test mock that captures subscription requests and allows driving
/// streams programmatically.
#[derive(Clone)]
pub struct MockStreamFactory {
    /// Every `SubscribeRequest` passed to `subscribe()` is recorded
    /// here so tests can assert on filter contents, commitment levels,
    /// etc.
    captured_requests: Arc<Mutex<Vec<SubscribeRequest>>>,

    /// Requests sent through a `MockStreamHandle::write()` call are
    /// recorded here so tests can verify handle-driven updates.
    handle_requests: Arc<Mutex<Vec<SubscribeRequest>>>,

    /// A sender that the test uses to push `LaserResult` items into
    /// the streams returned by `subscribe()`.
    /// Each call to `subscribe()` creates a new mpsc channel; the rx
    /// side becomes the returned stream, and the tx side is stored
    /// here so the test can drive updates.
    stream_senders: Arc<Mutex<Vec<Arc<mpsc::UnboundedSender<LaserResult>>>>>,
}

impl MockStreamFactory {
    /// Create a new mock stream factory
    pub fn new() -> Self {
        Self {
            captured_requests: Arc::new(Mutex::new(Vec::new())),
            handle_requests: Arc::new(Mutex::new(Vec::new())),
            stream_senders: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Get the captured subscription requests (from `subscribe()`)
    pub fn captured_requests(&self) -> Vec<SubscribeRequest> {
        self.captured_requests.lock().unwrap().clone()
    }

    /// Get the requests sent through stream handles (from
    /// `handle.write()`)
    pub fn handle_requests(&self) -> Vec<SubscribeRequest> {
        self.handle_requests.lock().unwrap().clone()
    }

    /// Push an error update to a specific stream
    pub fn push_error_to_stream(&self, idx: usize, error: LaserstreamError) {
        let senders = self.stream_senders.lock().unwrap();
        if let Some(sender) = senders.get(idx) {
            let _ = sender.send(Err(error));
        }
    }

    /// Push an update to a specific stream by index
    pub fn push_update_to_stream(&self, idx: usize, update: LaserResult) {
        let senders = self.stream_senders.lock().unwrap();
        if let Some(sender) = senders.get(idx) {
            let _ = sender.send(update);
        }
    }

    /// Get the number of active streams
    pub fn active_stream_count(&self) -> usize {
        self.stream_senders.lock().unwrap().len()
    }

    /// Close a specific stream by index
    pub fn close_stream(&self, idx: usize) {
        let mut senders = self.stream_senders.lock().unwrap();
        if idx < senders.len() {
            senders.remove(idx);
        }
    }
}

impl Default for MockStreamFactory {
    fn default() -> Self {
        Self::new()
    }
}

/// Mock handle that records write requests and drains them into the
/// shared `handle_requests` vec on the factory.
#[derive(Clone)]
pub struct MockStreamHandle {
    handle_requests: Arc<Mutex<Vec<SubscribeRequest>>>,
}

#[async_trait]
impl StreamHandle for MockStreamHandle {
    async fn write(
        &self,
        request: SubscribeRequest,
    ) -> Result<(), LaserstreamError> {
        self.handle_requests.lock().unwrap().push(request);
        Ok(())
    }
}

#[async_trait]
impl StreamFactory<MockStreamHandle> for MockStreamFactory {
    async fn subscribe(
        &self,
        request: SubscribeRequest,
    ) -> RemoteAccountProviderResult<LaserStreamWithHandle<MockStreamHandle>>
    {
        // Record the initial subscribe request
        self.captured_requests.lock().unwrap().push(request.clone());

        // Create a channel for driving LaserResult items into the
        // stream
        let (stream_tx, stream_rx) = mpsc::unbounded_channel::<LaserResult>();
        let stream = Box::pin(UnboundedReceiverStream::new(stream_rx));

        let stream_tx = Arc::new(stream_tx);
        self.stream_senders.lock().unwrap().push(stream_tx);

        // The handle shares the factory's handle_requests vec so
        // every write is visible to tests immediately.
        let handle = MockStreamHandle {
            handle_requests: Arc::clone(&self.handle_requests),
        };

        // Write the actual request to the handle (mirroring
        // production behaviour of sending it over the network).
        handle.write(request).await.unwrap();

        Ok(LaserStreamWithHandle { stream, handle })
    }
}
