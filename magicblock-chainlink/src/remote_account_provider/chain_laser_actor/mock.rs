use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use helius_laserstream::{
    grpc::{self, SubscribeRequest},
    LaserstreamError,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use super::{LaserResult, StreamFactory};
use crate::remote_account_provider::chain_laser_actor::{
    LaserStreamWithHandle, StreamHandle,
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

    /// Push a success update to all active streams
    pub fn push_success_to_all(&self, update: grpc::SubscribeUpdate) {
        let senders = self.stream_senders.lock().unwrap();
        for sender in senders.iter() {
            let _ = sender.send(Ok(update.clone()));
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

    /// Clear all state (requests, handle requests and streams)
    pub fn clear(&self) {
        self.captured_requests.lock().unwrap().clear();
        self.handle_requests.lock().unwrap().clear();
        self.stream_senders.lock().unwrap().clear();
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

impl StreamFactory<MockStreamHandle> for MockStreamFactory {
    fn subscribe(
        &self,
        request: SubscribeRequest,
    ) -> LaserStreamWithHandle<MockStreamHandle> {
        // Record the initial subscribe request
        self.captured_requests.lock().unwrap().push(request);

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

        LaserStreamWithHandle { stream, handle }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use helius_laserstream::grpc::{
        CommitmentLevel, SubscribeRequestFilterAccounts,
    };

    use super::*;

    #[test]
    fn test_mock_captures_requests() {
        let mock = MockStreamFactory::new();

        let mut accounts = HashMap::new();
        accounts.insert(
            "test".to_string(),
            SubscribeRequestFilterAccounts::default(),
        );

        let request = SubscribeRequest {
            accounts,
            commitment: Some(CommitmentLevel::Finalized.into()),
            ..Default::default()
        };

        let _stream = mock.subscribe(request.clone());

        let captured = mock.captured_requests();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].commitment, request.commitment);
    }

    #[tokio::test]
    async fn test_mock_handle_write_records_requests() {
        let mock = MockStreamFactory::new();

        let request = SubscribeRequest::default();
        let result = mock.subscribe(request);

        assert_eq!(mock.active_stream_count(), 1);

        // Write an updated request through the handle
        let mut accounts = HashMap::new();
        accounts.insert(
            "updated".to_string(),
            SubscribeRequestFilterAccounts::default(),
        );
        let update_request = SubscribeRequest {
            accounts,
            commitment: Some(CommitmentLevel::Confirmed.into()),
            ..Default::default()
        };

        result.handle.write(update_request.clone()).await.unwrap();

        let handle_reqs = mock.handle_requests();
        assert_eq!(handle_reqs.len(), 1);
        assert_eq!(handle_reqs[0].commitment, update_request.commitment);
        assert!(handle_reqs[0].accounts.contains_key("updated"));
    }

    #[tokio::test]
    async fn test_mock_handle_write_multiple() {
        let mock = MockStreamFactory::new();

        let r1 = mock.subscribe(SubscribeRequest::default());
        let r2 = mock.subscribe(SubscribeRequest::default());

        // Both handles share the same handle_requests vec
        r1.handle
            .write(SubscribeRequest {
                commitment: Some(CommitmentLevel::Processed.into()),
                ..Default::default()
            })
            .await
            .unwrap();

        r2.handle
            .write(SubscribeRequest {
                commitment: Some(CommitmentLevel::Finalized.into()),
                ..Default::default()
            })
            .await
            .unwrap();

        let handle_reqs = mock.handle_requests();
        assert_eq!(handle_reqs.len(), 2);
        assert_eq!(mock.captured_requests().len(), 2);
    }

    #[test]
    fn test_mock_can_clear() {
        let mock = MockStreamFactory::new();

        let request = SubscribeRequest::default();
        let _stream = mock.subscribe(request);

        assert_eq!(mock.captured_requests().len(), 1);

        mock.clear();

        assert_eq!(mock.captured_requests().len(), 0);
        assert_eq!(mock.handle_requests().len(), 0);
    }
}
