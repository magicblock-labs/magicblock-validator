use std::sync::Arc;

use tokio::sync::oneshot;

pub(crate) mod http;
pub(crate) mod websocket;

/// An RAII-based signal for coordinating graceful server shutdown.
///
/// This struct leverages the `Drop` trait to automatically send a completion signal
/// when all references to it have been dropped.
///
/// ## Pattern
///
/// An `Arc<Shutdown>` is created alongside a `Receiver`. The `Arc` is cloned and
/// distributed to all active tasks (e.g., connection handlers). The main server
/// task awaits the `Receiver`. When each task completes, its `Arc` is dropped.
/// When the final `Arc` (including the one held by the main server loop) is dropped,
/// the signal is sent, the `Receiver` resolves, and the server can exit cleanly.
struct Shutdown(Option<oneshot::Sender<()>>);

impl Shutdown {
    /// Creates a new shutdown signal.
    ///
    /// # Returns
    ///
    /// A tuple containing:
    /// 1.  An `Arc<Shutdown>` which acts as the distributable RAII guard.
    /// 2.  A `Receiver<()>` which can be awaited to detect when all guards have been dropped.
    fn new() -> (Arc<Self>, oneshot::Receiver<()>) {
        let (tx, rx) = oneshot::channel();
        (Self(Some(tx)).into(), rx)
    }
}

impl Drop for Shutdown {
    /// When the `Shutdown` instance is dropped, it sends the completion signal.
    fn drop(&mut self) {
        self.0.take().map(|tx| tx.send(()));
    }
}
