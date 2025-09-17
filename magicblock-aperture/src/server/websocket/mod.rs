//! Defines the WebSocket server task that manages connection lifecycles.

use std::sync::Arc;

use connection::{ConnectionHandler, WebsocketStream};
use hyper::{
    header::{CONNECTION, UPGRADE},
    Request,
};
use tokio::sync::{mpsc::Receiver, oneshot};
use tokio_util::sync::CancellationToken;

use crate::state::{
    subscriptions::SubscriptionsDb, transactions::TransactionsCache,
    SharedState,
};

use super::Shutdown;

/// The main WebSocket server task.
///
/// Receives established streams from a channel (sent by HttpServer)
/// and spawns a `ConnectionHandler` for each, managing the graceful
/// shutdown of all active connections.
pub struct WebsocketServer {
    /// Receives established WebSocket streams from the HTTP server.
    streams: Receiver<WebsocketStream>,
    /// State template cloned into each new connection handler.
    state: ConnectionState,
    /// Awaited during shutdown to ensure all connection handlers have terminated.
    shutdown: oneshot::Receiver<()>,
}

/// A container of shared state cloned into each new `ConnectionHandler`.
#[derive(Clone)]
struct ConnectionState {
    /// Handle to the central subscription database.
    subscriptions: SubscriptionsDb,
    /// Handle to the recent transactions cache.
    transactions: TransactionsCache,
    /// Global cancellation token for graceful shutdown.
    cancel: CancellationToken,
    /// RAII guard that participates in graceful shutdown.
    shutdown: Arc<Shutdown>,
}

impl WebsocketServer {
    /// Prepares the WebSocket server by creating the shared connection state.
    pub(crate) async fn new(
        streams: Receiver<WebsocketStream>,
        state: &SharedState,
    ) -> Self {
        let (shutdown, rx) = Shutdown::new();
        let state = ConnectionState {
            subscriptions: state.subscriptions.clone(),
            transactions: state.transactions.clone(),
            cancel: state.cancel.clone(),
            shutdown,
        };
        Self {
            streams,
            state,
            shutdown: rx,
        }
    }

    /// Runs the main server loop, accepting new streams and spawning handlers.
    ///
    /// This future completes once the cancellation token is triggered and all
    /// active connection handlers have shut down gracefully.
    pub(crate) async fn run(mut self) {
        loop {
            tokio::select! {
                Some(stream) = self.streams.recv() => {
                    let state = self.state.clone();
                    tokio::spawn(ConnectionHandler::new(stream, state).run());
                },
                _ = self.state.cancel.cancelled() => break,
            }
        }
        drop(self.state);
        let _ = self.shutdown.await;
    }
}

/// Checks if an HTTP request is a valid WebSocket upgrade request.
///
/// It inspects the `Connection` and `Upgrade` headers for the required values.
pub(crate) fn is_websocket_upgrade(
    req: &Request<impl hyper::body::Body>,
) -> bool {
    let compare = |h: &str, v| h.trim().eq_ignore_ascii_case(v);

    let is_upgrade_header_correct = req
        .headers()
        .get(UPGRADE)
        .and_then(|val| val.to_str().ok())
        .map_or(false, |s| compare(s, "websocket"));

    if !is_upgrade_header_correct {
        return false;
    }

    req.headers()
        .get(CONNECTION)
        .and_then(|val| val.to_str().ok())
        .map_or(false, |s| s.split(',').any(|s| compare(s, "Upgrade")))
}

/// Manages the lifecycle and I/O of a single WebSocket connection.
pub(crate) mod connection;
/// Handles RPC method dispatch and subscriptions for a connection.
pub(crate) mod dispatch;
