use engine::Engine;
use json::Value;
use std::time::Duration;
use tracing::{debug, warn};

use fastwebsockets::{CloseCode, Frame, OpCode, WebSocket, WebSocketError, WebSocketWrite};
use hyper::{body::Bytes, upgrade::Upgraded};
use hyper_util::rt::TokioIo;
use tokio::{
    io::AsyncWrite,
    sync::mpsc::{self, Receiver},
    time::{self, Instant},
};
use tokio_util::sync::CancellationToken;

use super::dispatch::WsDispatcher;
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
/// - Observing connection cancellation and server shutdown between events.
pub(super) struct ConnectionHandler {
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
        let dispatcher = WsDispatcher::new(engine, tx, cancel.child_token());
        Self { dispatcher, ws, updates_rx }
    }

    /// The main event loop for the WebSocket connection.
    ///
    /// This long-running task uses `tokio::select!` to concurrently handle multiple
    /// asynchronous events:
    /// - **Incoming client messages**: Parses and dispatches RPC requests.
    /// - **Outgoing subscription notifications**: Pushes updates from the server to the client.
    /// - **Keep-alive**: Sends periodic pings and closes the connection if it becomes inactive.
    /// - **Cancellation**: Listens for subscription closure or server shutdown.
    ///
    /// The loop terminates on I/O failure, timeout, or cancellation.
    pub(super) async fn run(self) {
        const MAX_INACTIVE_INTERVAL: Duration = Duration::from_secs(60);
        const PING_PERIOD: Duration = Duration::from_secs(30);
        let Self {
            ws,
            mut dispatcher,
            mut updates_rx,
        } = self;
        let (mut reader, mut writer) = ws.split(tokio::io::split);
        let cancel = dispatcher.cancel.clone();
        reader.set_auto_pong(false);
        let mut last_activity = Instant::now();
        let mut last_request = Value::Null;
        let mut replies = 0_u64;
        let next_ping = time::sleep_until(Instant::now() + PING_PERIOD);
        tokio::pin!(next_ping);

        'connection: loop {
            // Reading consumes frame headers before awaiting the remaining bytes.
            // Keep that future alive while writing notifications or keepalives.
            let mut close_reply: Option<Frame<'static>> = None;
            let result = {
                // Preserve the parser's close validation and flush its reply through
                // the single writer once reading completes. Pongs are handled below.
                let mut defer_close = |frame| {
                    close_reply = Some(frame);
                    std::future::ready(Ok::<_, WebSocketError>(()))
                };
                let read = reader.read_frame(&mut defer_close);
                tokio::pin!(read);
                loop {
                    tokio::select! {
                        // Prioritize cancellation even while a frame is incomplete.
                        biased;
                        _ = cancel.cancelled() => break 'connection,
                        result = &mut read => break result,
                        _ = &mut next_ping => {
                            if last_activity.elapsed() > MAX_INACTIVE_INTERVAL {
                                let frame = Frame::close(
                                    CloseCode::Policy.into(),
                                    b"connection inactive for too long"
                                );
                                let _ = write_frame(&mut writer, frame).await;
                                return;
                            }
                            let frame = Frame::new(true, OpCode::Ping, None, b"".as_ref().into());
                            if let Err(error) = write_frame(&mut writer, frame).await {
                                warn!(?error, %last_request, replies, "WebSocket ping failed");
                                return;
                            };
                            next_ping.as_mut().reset(Instant::now() + PING_PERIOD);
                        }

                        Some(update) = updates_rx.recv() => {
                            if let Err(error) = write_frame(&mut writer, Frame::text(update.as_ref().into())).await {
                                warn!(?error, %last_request, replies, "WebSocket notification failed");
                                return;
                            }
                        }
                    }
                }
            };
            if let Some(reply) = close_reply
                && write_frame(&mut writer, reply).await.is_err()
            {
                return;
            }
            let frame = match result {
                Ok(frame) => frame,
                Err(error) => {
                    if matches!(error, WebSocketError::UnexpectedEOF) {
                        debug!(?error, "WebSocket peer disconnected");
                    } else {
                        warn!(?error, %last_request, replies, "WebSocket read failed");
                    }
                    break;
                }
            };
            last_activity = Instant::now();
            next_ping.as_mut().reset(Instant::now() + PING_PERIOD);
            match frame.opcode {
                OpCode::Close => return,
                OpCode::Ping => {
                    if write_frame(&mut writer, Frame::pong(frame.payload)).await.is_err() {
                        return;
                    }
                    continue;
                }
                OpCode::Text | OpCode::Binary => {}
                _ => continue,
            }
            let parsed = json::from_slice(&frame.payload).map_err(RpcError::parse_error);
            let response = match parsed {
                Ok(request) => {
                    let result = dispatcher.dispatch(&request).await;
                    last_request = request.id;
                    result
                        .and_then(|result| {
                            ResponsePayload::encode_no_context(&last_request, result)
                        })
                        .or_else(|error| ResponseErrorPayload::encode(Some(&last_request), error))
                }
                Err(error) => ResponseErrorPayload::encode(None, error),
            };
            let payload = match response {
                Ok(payload) => payload,
                Err(error) => {
                    warn!(?error, "failed to serialize WebSocket error");
                    break;
                }
            };
            if let Err(error) = write_frame(&mut writer, Frame::text(payload.0.into())).await {
                warn!(?error, %last_request, replies, "WebSocket reply failed");
                return;
            }
            replies += 1;
        }
        drop(dispatcher);
        let close = Frame::close(
            CloseCode::Away.into(),
            b"connection closing; reconnect and refetch",
        );
        let _ = write_frame(&mut writer, close).await;
    }
}

/// Bounds backpressure on every frame, including the best-effort close handshake.
/// After any failure the caller must drop the socket: a frame may be partially written.
async fn write_frame(
    writer: &mut WebSocketWrite<impl AsyncWrite + Unpin>,
    frame: Frame<'_>,
) -> Result<(), WebSocketError> {
    const WRITE_TIMEOUT: Duration = Duration::from_secs(3);
    time::timeout(WRITE_TIMEOUT, writer.write_frame(frame)).await.map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::TimedOut, "WebSocket write timed out")
    })?
}
