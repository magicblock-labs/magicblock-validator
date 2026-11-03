use connection::ConnectionHandler;
use engine::Engine;
use fastwebsockets::upgrade::upgrade;
use http_body_util::Empty;
use hyper::{
    Request, Response,
    body::{Bytes, Incoming},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::{RpcResult, error::RpcError};

const MAX_BODY_SIZE: usize = 1024 * 1024;

/// The main WebSocket server.
///
/// This server listens for TCP connections and manages the HTTP Upgrade handshake
/// to establish persistent WebSocket connections for real-time event subscriptions.
/// On shutdown, it stops accepting new connections without waiting for active
/// WebSocket tasks to drain.
pub(crate) struct WebsocketServer {
    /// The TCP listener that accepts new client connections.
    socket: TcpListener,

    /// A handle to the engine, used to open per-subscription update streams.
    engine: Engine,
    /// Cancels both pending upgrades and established connections.
    cancel: CancellationToken,
}

impl WebsocketServer {
    /// Takes ownership of a bound listener and the connection dependencies.
    pub(crate) fn new(socket: TcpListener, engine: Engine, cancel: CancellationToken) -> Self {
        Self { socket, engine, cancel }
    }

    /// Starts the main server loop to accept and handle incoming connections.
    ///
    /// When the server's `cancel` token is triggered, the loop stops accepting
    /// new connections and returns immediately so validator restart time is not
    /// blocked by active WebSocket connections.
    pub(crate) async fn run(self) {
        loop {
            tokio::select! {
                // A new client is attempting to connect.
                Ok((stream, _)) = self.socket.accept() => {
                    self.handle(stream);
                },
                // The server shutdown signal has been received.
                _ = self.cancel.cancelled() => break,
            }
        }
    }

    /// Spawns a task to handle a new TCP stream as a potential WebSocket connection.
    ///
    /// This function sets up a Hyper service to perform the initial HTTP Upgrade handshake.
    fn handle(&self, stream: TcpStream) {
        let engine = self.engine.clone();
        let cancel = self.cancel.clone();
        let connection_cancel = cancel.clone();
        let io = TokioIo::new(stream);
        let handler = service_fn(move |request| {
            handle_upgrade(request, engine.clone(), connection_cancel.clone())
        });

        tokio::spawn(async move {
            let builder = http1::Builder::new();
            // The `with_upgrades` method enables Hyper to handle the WebSocket upgrade protocol.
            let connection = builder.serve_connection(io, handler).with_upgrades();
            tokio::select! {
                result = connection => if let Err(error) = result { warn!(?error, "WebSocket connection terminated"); },
                _ = cancel.cancelled() => {},
            }
        });
    }
}

/// A Hyper service function that handles an incoming HTTP request
/// and attempts to upgrade it to a WebSocket connection.
async fn handle_upgrade(
    request: Request<Incoming>,
    engine: Engine,
    cancel: CancellationToken,
) -> RpcResult<Response<Empty<Bytes>>> {
    // `fastwebsockets::upgrade` checks the request headers (e.g., `Connection: upgrade`).
    // If valid, it returns the "101 Switching Protocols" response and a future that
    // will resolve to the established WebSocket stream.
    let (response, ws) = upgrade(request).map_err(RpcError::internal)?;

    // Spawn a new task to manage the WebSocket communication, freeing up the
    // Hyper service to handle other potential incoming connections.
    tokio::spawn(async move {
        let result = tokio::select! {
            result = ws => result,
            _ = cancel.cancelled() => return,
        };
        let mut ws = match result {
            Ok(ws) => ws,
            Err(e) => {
                warn!(
                    error = ?e,
                    "HTTP upgrade to WebSocket failed"
                );
                return;
            }
        };
        ws.set_max_message_size(MAX_BODY_SIZE);
        // The `ConnectionHandler` will now take over the WebSocket stream.
        let handler = ConnectionHandler::new(ws, engine, cancel);
        handler.run().await
    });

    // Return the "101 Switching Protocols" response to the client.
    Ok(response)
}

pub(crate) mod connection;
pub(crate) mod dispatch;
