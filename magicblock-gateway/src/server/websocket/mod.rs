use std::{net::SocketAddr, sync::Arc};

use connection::ConnectionHandler;
use fastwebsockets::upgrade::upgrade;
use http_body_util::Empty;
use hyper::{
    body::{Bytes, Incoming},
    server::conn::http1,
    service::service_fn,
    Request, Response,
};
use hyper_util::rt::TokioIo;
use log::warn;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot::Receiver,
};
use tokio_util::sync::CancellationToken;

use crate::{
    error::RpcError,
    state::{
        subscriptions::SubscriptionsDb, transactions::TransactionsCache,
        SharedState,
    },
    RpcResult,
};

use super::Shutdown;

/// The main WebSocket server.
///
/// This server listens for TCP connections and manages the HTTP Upgrade handshake
/// to establish persistent WebSocket connections for real-time event subscriptions.
/// It supports graceful shutdown to ensure all client connections are terminated cleanly.
pub struct WebsocketServer {
    /// The TCP listener that accepts new client connections.
    socket: TcpListener,
    /// The shared state required by each individual connection handler.
    state: ConnectionState,
    /// The receiving end of the shutdown signal, used to wait for all
    /// active connections to terminate before the server fully exits.
    shutdown: Receiver<()>,
}

/// A container for shared state that is cloned for each new WebSocket connection.
///
/// This serves as a dependency container, providing each connection handler with
/// the necessary context to process requests and manage subscriptions.
#[derive(Clone)]
struct ConnectionState {
    /// A handle to the central subscription database.
    subscriptions: SubscriptionsDb,
    /// A handle to the cache of recent transactions.
    transactions: TransactionsCache,
    /// The global cancellation token for shutting down the server.
    cancel: CancellationToken,
    /// An RAII guard for tracking outstanding connections to enable graceful shutdown.
    shutdown: Arc<Shutdown>,
}

impl WebsocketServer {
    /// Initializes the WebSocket server by binding a TCP
    /// listener and preparing the shared connection state.
    pub(crate) async fn new(
        addr: SocketAddr,
        state: &SharedState,
        cancel: CancellationToken,
    ) -> RpcResult<Self> {
        let socket =
            TcpListener::bind(addr).await.map_err(RpcError::internal)?;
        let (shutdown, rx) = Shutdown::new();
        let state = ConnectionState {
            subscriptions: state.subscriptions.clone(),
            transactions: state.transactions.clone(),
            cancel,
            shutdown,
        };
        Ok(Self {
            socket,
            state,
            shutdown: rx,
        })
    }

    /// Starts the main server loop to accept and handle incoming connections.
    ///
    /// ## Graceful Shutdown
    /// When the server's `cancel` token is triggered, the loop stops accepting new
    /// connections. It then waits for all active connections to complete their work
    /// and drop their `Shutdown` handles before the method returns and the server exits.
    pub(crate) async fn run(mut self) {
        loop {
            tokio::select! {
                // A new client is attempting to connect.
                Ok((stream, _)) = self.socket.accept() => {
                    self.handle(stream);
                },
                // The server shutdown signal has been received.
                _ = self.state.cancel.cancelled() => break,
            }
        }
        // Drop the main `ConnectionState` which holds the original `Shutdown` handle.
        drop(self.state);
        // Wait for all spawned connection tasks to finish.
        let _ = self.shutdown.await;
    }

    /// Spawns a task to handle a new TCP stream as a potential WebSocket connection.
    ///
    /// This function sets up a Hyper service to perform the initial HTTP Upgrade handshake.
    fn handle(&mut self, stream: TcpStream) {
        // Clone the state for the new connection. This includes cloning the Arc<Shutdown>
        // handle, incrementing the in-flight connection count.
        let state = self.state.clone();

        let io = TokioIo::new(stream);
        let handler =
            service_fn(move |request| handle_upgrade(request, state.clone()));

        tokio::spawn(async move {
            let builder = http1::Builder::new();
            // The `with_upgrades` method enables Hyper to handle the WebSocket upgrade protocol.
            let connection =
                builder.serve_connection(io, handler).with_upgrades();
            if let Err(error) = connection.await {
                warn!("websocket connection terminated with error: {error}");
            }
        });
    }
}

/// A Hyper service function that handles an incoming HTTP request
/// and attempts to upgrade it to a WebSocket connection.
async fn handle_upgrade(
    request: Request<Incoming>,
    state: ConnectionState,
) -> RpcResult<Response<Empty<Bytes>>> {
    // `fastwebsockets::upgrade` checks the request headers (e.g., `Connection: upgrade`).
    // If valid, it returns the "101 Switching Protocols" response and a future that
    // will resolve to the established WebSocket stream.
    let (response, ws) = upgrade(request).map_err(RpcError::internal)?;

    // Spawn a new task to manage the WebSocket communication, freeing up the
    // Hyper service to handle other potential incoming connections.
    tokio::spawn(async move {
        let Ok(ws) = ws.await else {
            warn!("failed http upgrade to ws connection");
            return;
        };
        // The `ConnectionHandler` will now take over the WebSocket stream.
        let handler = ConnectionHandler::new(ws, state);
        handler.run().await
    });

    // Return the "101 Switching Protocols" response to the client.
    Ok(response)
}

pub(crate) mod connection;
pub(crate) mod dispatch;
