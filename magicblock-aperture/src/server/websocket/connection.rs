use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};

use fastwebsockets::{
    CloseCode, Frame, OpCode, Payload, WebSocket, WebSocketError,
};
use hyper::{body::Bytes, upgrade::Upgraded};
use hyper_util::rt::TokioIo;
use json::Value;
use log::debug;
use tokio::{
    sync::mpsc::{self, Receiver},
    time::{self, Instant},
};
use tokio_util::sync::CancellationToken;

use super::{
    dispatch::{WsDispatchResult, WsDispatcher},
    ConnectionState,
};
use crate::{
    error::RpcError,
    requests::payload::{ResponseErrorPayload, ResponsePayload},
    server::{websocket::dispatch::WsConnectionChannel, Shutdown},
};

/// A type alias for the underlying WebSocket stream provided by `fastwebsockets`.
type WebsocketStream = WebSocket<TokioIo<Upgraded>>;
/// A type alias for a unique identifier assigned to each WebSocket connection.
pub(crate) type ConnectionID = u32;

/// Manages the lifecycle and bi-directional communication of a single WebSocket connection.
///
/// This handler is responsible for:
/// - Reading and parsing RPC requests from the client.
/// - Dispatching requests to the `WsDispatcher` for processing.
/// - Receiving subscription notifications from various events and pushing them to the client.
/// - Handling keep-alive pings and detecting inactive connections.
/// - Participating in the server's graceful shutdown mechanism.
pub(super) struct ConnectionHandler {
    /// The server's global cancellation token for graceful shutdown.
    cancel: CancellationToken,
    /// The underlying WebSocket stream for reading and writing frames.
    ws: WebsocketStream,
    /// The request dispatcher for this specific connection. It manages all active
    /// subscriptions for this client.
    dispatcher: WsDispatcher,
    /// A channel for receiving subscription updates (e.g., account changes, slot updates)
    /// from the server's background `EventProcessor`s.
    updates_rx: Receiver<Bytes>,
    /// A clone of the server's `Shutdown` handle. Its presence in this struct ensures
    /// that the server will not fully shut down until this connection is terminated.
    _sd: Arc<Shutdown>,
}

impl ConnectionHandler {
    /// Creates a new handler for an established WebSocket connection.
    ///
    /// This function generates a unique ID and creates a dedicated MPSC channel for this
    /// connection, which is used to push subscription notifications from the EventProcessor.
    pub(super) fn new(ws: WebsocketStream, state: ConnectionState) -> Self {
        static CONNECTION_COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = CONNECTION_COUNTER.fetch_add(1, Ordering::Relaxed);

        // Create a dedicated channel for this connection to receive updates.
        let (tx, updates_rx) = mpsc::channel(4096);
        let chan = WsConnectionChannel { id, tx };

        // The dispatcher is tied to this specific connection via its channel.
        let dispatcher =
            WsDispatcher::new(state.subscriptions, state.transactions, chan);
        Self {
            dispatcher,
            cancel: state.cancel,
            ws,
            updates_rx,
            _sd: state.shutdown,
        }
    }

    /// The main event loop for the WebSocket connection.
    ///
    /// This long-running task uses `tokio::select!` to concurrently handle multiple
    /// asynchronous events:
    /// - **Incoming client messages**: Parses and dispatches RPC requests.
    /// - **Outgoing subscription notifications**: Pushes updates from the server to the client.
    /// - **Keep-alive**: Sends periodic pings and closes the connection if it becomes inactive.
    /// - **Shutdown**: Listens for the global server shutdown signal.
    ///
    /// The loop terminates upon any I/O error, an inactivity timeout, or a shutdown signal.
    pub(super) async fn run(mut self) {
        const MAX_INACTIVE_INTERVAL: Duration = Duration::from_secs(60);
        const PING_PERIOD: Duration = Duration::from_secs(30);
        let mut last_activity = Instant::now();
        let next_ping = time::sleep_until(Instant::now() + PING_PERIOD);
        tokio::pin!(next_ping);

        loop {
            tokio::select! {
                // Prioritize reading frames from the client.
                biased;

                // 1. Handle an incoming frame from the client's WebSocket.
                Ok(frame) = self.ws.read_frame() => {
                    // Record inbound client activity
                    last_activity = Instant::now();
                    // Reschedule the next ping
                    next_ping.as_mut().reset(Instant::now() + PING_PERIOD);

                    if frame.opcode != OpCode::Text {
                        continue;
                    }

                    // Parse the JSON RPC request.
                    let parsed = json::from_slice(&frame.payload).map_err(RpcError::parse_error);
                    let mut request = match parsed {
                        Ok(r) => r,
                        Err(error) => {
                            // Even on error, we attempted to respond; keep pings scheduled after this activity
                            let _ = self.report_failure(None, error).await;
                            continue;
                        }
                    };

                    // Dispatch the request and report the outcome to the client.
                    let success = match self.dispatcher.dispatch(&mut request).await {
                        Ok(r) => self.report_success(r).await,
                        Err(e) => self.report_failure(Some(&request.id), e).await,
                    };

                    // If we fail to send the response, terminate the connection.
                    if !success { break };
                }

                // 2. Handle the periodic keep-alive timer (scheduled relative to last activity).
                _ = &mut next_ping => {
                    // If the connection has been idle for too long, close it.
                    if last_activity.elapsed() > MAX_INACTIVE_INTERVAL {
                        let frame = Frame::close(
                            CloseCode::Policy.into(),
                            b"connection inactive for too long"
                        );
                        let _ = self.ws.write_frame(frame).await;
                        break;
                    }
                    // Otherwise, send a standard WebSocket PING frame.
                    let frame = Frame::new(true, OpCode::Ping, None, b"".as_ref().into());
                    if self.ws.write_frame(frame).await.is_err() {
                        break;
                    };
                    // Schedule the next ping
                    next_ping.as_mut().reset(Instant::now() + PING_PERIOD);
                }

                // 3. Handle a new subscription notification from the server backend.
                Some(update) = self.updates_rx.recv() => {
                    if self.send(update.as_ref()).await.is_err() {
                        break;
                    }
                }

                // 4. Handle the global server shutdown signal.
                _ = self.cancel.cancelled() => break,

                // 5. Run cleanup logic for this connection (e.g., an expiring sub).
                _ = self.dispatcher.cleanup() => {}

                else => {
                    break;
                }
            }
        }
        // send a close frame (best effort) to the client
        let frame =
            Frame::close(CloseCode::Away.into(), b"server is shutting down");
        let _ = self.ws.write_frame(frame).await;
    }

    /// Formats and sends a standard JSON-RPC success response to the client.
    async fn report_success(&mut self, result: WsDispatchResult) -> bool {
        let payload =
            ResponsePayload::encode_no_context_raw(&result.id, result.result);
        self.send(payload.0).await.is_ok()
    }

    /// Formats and sends a standard JSON-RPC error response to the client.
    async fn report_failure(
        &mut self,
        id: Option<&Value>,
        error: RpcError,
    ) -> bool {
        let payload = ResponseErrorPayload::encode(id, error);
        self.send(payload.into_body().0).await.is_ok()
    }

    /// A low-level helper to write a payload as a WebSocket text frame.
    #[inline]
    async fn send(
        &mut self,
        payload: impl Into<Payload<'_>>,
    ) -> Result<(), WebSocketError> {
        let frame = Frame::text(payload.into());
        self.ws.write_frame(frame).await.inspect_err(|e| {
            debug!("failed to send websocket frame to the client: {e}")
        })
    }
}
