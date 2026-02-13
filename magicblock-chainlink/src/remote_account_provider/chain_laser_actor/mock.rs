use std::sync::{Arc, Mutex};

use helius_laserstream::grpc::SubscribeRequest;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use super::{LaserResult, LaserStream, StreamFactory};

/// A test mock that captures subscription requests and allows driving streams
/// programmatically
#[derive(Clone)]
#[allow(dead_code)]
pub struct MockStreamFactory {
    /// Every SubscribeRequest passed to `subscribe()` is recorded here
    /// so tests can assert on filter contents, commitment levels, etc.
    captured_requests: Arc<Mutex<Vec<SubscribeRequest>>>,

    /// A sender that the test uses to push `LaserResult` items into the
    /// streams returned by `subscribe()`.
    /// Each call to `subscribe()` creates a new mpsc channel; the rx side
    /// becomes the returned stream, and the tx side is stored here so the
    /// test can drive updates.
    stream_senders: Arc<Mutex<Vec<mpsc::UnboundedSender<LaserResult>>>>,
}

#[allow(dead_code)]
impl MockStreamFactory {
    /// Create a new mock stream factory
    pub fn new() -> Self {
        Self {
            captured_requests: Arc::new(Mutex::new(Vec::new())),
            stream_senders: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Get the captured subscription requests
    pub fn captured_requests(&self) -> Vec<SubscribeRequest> {
        self.captured_requests.lock().unwrap().clone()
    }

    /// Push an error update to a specific stream
    pub fn push_error_to_stream(
        &self,
        idx: usize,
        error: helius_laserstream::LaserstreamError,
    ) {
        let senders = self.stream_senders.lock().unwrap();
        if let Some(sender) = senders.get(idx) {
            let _ = sender.send(Err(error));
        }
    }

    /// Push a success update to all active streams
    pub fn push_success_to_all(
        &self,
        update: helius_laserstream::grpc::SubscribeUpdate,
    ) {
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

    /// Clear all state (requests and streams)
    pub fn clear(&self) {
        self.captured_requests.lock().unwrap().clear();
        self.stream_senders.lock().unwrap().clear();
    }
}

impl Default for MockStreamFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamFactory for MockStreamFactory {
    fn subscribe(&self, request: SubscribeRequest) -> LaserStream {
        // Record the request
        self.captured_requests.lock().unwrap().push(request);

        // Create a channel and store the sender
        let (tx, rx) = mpsc::unbounded_channel();
        self.stream_senders.lock().unwrap().push(tx);

        // Return the receiver wrapped as a stream
        Box::pin(UnboundedReceiverStream::new(rx))
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
    async fn test_mock_can_drive_updates() {
        let mock = MockStreamFactory::new();

        let request = SubscribeRequest::default();
        let _stream = mock.subscribe(request);

        assert_eq!(mock.active_stream_count(), 1);

        // The stream is created but we can't easily test the update without
        // running the actual stream, which is tested in integration tests
    }

    #[test]
    fn test_mock_can_clear() {
        let mock = MockStreamFactory::new();

        let request = SubscribeRequest::default();
        let _stream = mock.subscribe(request);

        assert_eq!(mock.captured_requests().len(), 1);

        mock.clear();

        assert_eq!(mock.captured_requests().len(), 0);
    }
}
