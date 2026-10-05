use engine::Engine;
use std::time::Duration;
use tracing::warn;

use fastwebsockets::{CloseCode, Frame, OpCode, Payload, WebSocket, WebSocketError};
use hyper::{body::Bytes, upgrade::Upgraded};
use hyper_util::rt::TokioIo;
use json::Value;
use tokio::{
    sync::mpsc::{self, Receiver},
    time::{self, Instant},
};
use tokio_util::sync::CancellationToken;

use super::dispatch::{SubResult, WsDispatcher};
use crate::{
    error::RpcError,
    requests::payload::{ResponseErrorPayload, ResponsePayload},
};

/// A type alias for the underlying WebSocket stream provided by `fastwebsockets`.
type WebsocketStream = WebSocket<TokioIo<Upgraded>>;

/// Manages the lifecycle and bi-directional communication of a single WebSocket connection.
///
/// This handler is responsible for:
/// - Reading and parsing RPC requests from the client.
/// - Dispatching requests to the `WsDispatcher` for processing.
/// - Receiving subscription notifications from various events and pushing them to the client.
/// - Handling keep-alive pings and detecting inactive connections.
/// - Observing the server's shutdown token between connection events.
pub(super) struct ConnectionHandler {
    /// The server's global cancellation token.
    cancel: CancellationToken,
    /// The underlying WebSocket stream for reading and writing frames.
    ws: WebsocketStream,
    /// The request dispatcher for this specific connection. It manages all active
    /// subscriptions for this client.
    dispatcher: WsDispatcher,
    /// Bounded notification queue fed by this connection's subscription tasks.
    updates_rx: Receiver<Bytes>,
}

impl ConnectionHandler {
    /// Creates a new handler for an established WebSocket connection.
    ///
    /// Subscription tasks belong to its dispatcher and are aborted when it drops.
    pub(super) fn new(ws: WebsocketStream, engine: Engine, cancel: CancellationToken) -> Self {
        let (tx, updates_rx) = mpsc::channel(4096);

        // The dispatcher is tied to this specific connection via its channel.
        let dispatcher = WsDispatcher::new(engine, tx);
        Self {
            dispatcher,
            cancel,
            ws,
            updates_rx,
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
                // Prioritize fast system shutdown
                biased;

                // 0. We force shutdown the connection, without close frame
                _ = self.cancel.cancelled() => break,


                // 1. Handle an incoming frame from the client's WebSocket.
                Ok(frame) = self.ws.read_frame() => {
                    // Record inbound client activity
                    last_activity = Instant::now();
                    // Reschedule the next ping
                    next_ping.as_mut().reset(Instant::now() + PING_PERIOD);

                    if frame.opcode != OpCode::Text && frame.opcode != OpCode::Binary {
                        continue;
                    }

                    // Parse the JSON RPC request.
                    let parsed = json::from_slice(&frame.payload).map_err(RpcError::parse_error);
                    let request = match parsed {
                        Ok(r) => r,
                        Err(error) => {
                            let _ = self.report_failure(None, error).await;
                            continue;
                        }
                    };

                    // Dispatch the request and report the outcome to the client.
                    let success = match self.dispatcher.dispatch(&request).await {
                        Ok(r) => self.report_success(&request.id, r).await,
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

                // 3. Handle a new subscription notification from a forwarding task.
                Some(update) = self.updates_rx.recv() => {
                    if self.send(update.as_ref()).await.is_err() {
                        break;
                    }
                }

                else => {
                    break;
                }
            }
        }
        // send a close frame (best effort) to the client
        let frame = Frame::close(CloseCode::Away.into(), b"server is shutting down");
        let _ = self.ws.write_frame(frame).await;
    }

    /// Formats and sends a standard JSON-RPC success response to the client.
    async fn report_success(&mut self, id: &Value, result: SubResult) -> bool {
        match ResponsePayload::encode_no_context(id, result) {
            Ok(payload) => self.send(payload.0).await.is_ok(),
            Err(error) => self.report_failure(Some(id), error).await,
        }
    }

    async fn report_failure(&mut self, id: Option<&Value>, error: RpcError) -> bool {
        match ResponseErrorPayload::encode(id, error) {
            Ok(payload) => self.send(payload.0).await.is_ok(),
            Err(error) => {
                warn!(?error, "failed to serialize WebSocket error");
                false
            }
        }
    }

    /// A low-level helper to write a payload as a WebSocket text frame.
    #[inline]
    async fn send(&mut self, payload: impl Into<Payload<'_>>) -> Result<(), WebSocketError> {
        let frame = Frame::text(payload.into());
        self.ws.write_frame(frame).await
    }
}
