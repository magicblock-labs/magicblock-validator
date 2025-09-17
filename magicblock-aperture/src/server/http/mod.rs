use std::sync::Arc;

use dispatch::HttpDispatcher;
use hyper::service::service_fn;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn,
};
use service::RequestHandler;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc::Sender, oneshot::Receiver},
};
use tokio_util::sync::CancellationToken;

use magicblock_core::link::DispatchEndpoints;

use crate::{state::SharedState, RpcResult};

use super::{websocket::connection::WebsocketStream, Shutdown};

/// A graceful, Tokio-based server for handling RPC requests.
///
/// This server accepts TCP connections and uses a `RequestHandler` to serve them.
/// It discriminates between standard HTTP requests, which are handled directly,
/// and WebSocket upgrade requests, which are forwarded to a dedicated handler.
/// It also supports graceful shutdown to ensure in-flight requests are completed.
pub(crate) struct HttpServer {
    /// The TCP listener that accepts incoming connections.
    socket: TcpListener,
    /// A dispatcher for standard, non-upgrade HTTP requests.
    dispatcher: Arc<HttpDispatcher>,
    /// A channel sender to pass upgraded WebSocket connections to the `WebsocketServer`.
    websocket: Sender<WebsocketStream>,
    /// The main cancellation token. When triggered, the server stops accepting new connections.
    cancel: CancellationToken,
    /// A shared RAII guard for tracking in-flight connections. When all clones
    /// of this `Arc` are dropped, the `shutdown_rx` receiver is notified.
    shutdown: Arc<Shutdown>,
    /// The receiving end of the shutdown signal, used to wait for all connections to terminate.
    shutdown_rx: Receiver<()>,
}

impl HttpServer {
    /// Initializes the HTTP server and sets up shutdown signaling.
    pub(crate) async fn new(
        socket: TcpListener,
        state: SharedState,
        dispatch: &DispatchEndpoints,
        websocket: Sender<WebsocketStream>,
    ) -> RpcResult<Self> {
        let (shutdown, shutdown_rx) = Shutdown::new();

        Ok(Self {
            socket,
            cancel: state.cancel.clone(),
            dispatcher: HttpDispatcher::new(state, dispatch),
            websocket,
            shutdown,
            shutdown_rx,
        })
    }

    /// Starts the main server loop, accepting connections until a shutdown signal is received.
    pub(crate) async fn run(mut self) {
        loop {
            tokio::select! {
                biased;
                // Accept a new incoming connection.
                Ok((stream, _)) = self.socket.accept() => self.handle(stream),
                // Or, break the loop if the cancellation token is triggered.
                _ = self.cancel.cancelled() => break,
            }
        }

        // Stop accepting new connections and begin the graceful shutdown process.
        // Drop the main shutdown handle. The server will not exit until all connection
        // tasks have also dropped their handles.
        drop(self.shutdown);
        // Wait for the shutdown signal, which fires when all connections are closed.
        let _ = self.shutdown_rx.await;
    }

    /// Spawns a new task to handle a single incoming TCP connection.
    fn handle(&mut self, stream: TcpStream) {
        // Create a child token so this specific connection can be cancelled.
        let cancel = self.cancel.child_token();

        let io = TokioIo::new(stream);
        let handler = RequestHandler::new(self);
        let service =
            service_fn(move |request| handler.clone().handle(request));
        let shutdown = self.shutdown.clone();

        tokio::spawn(async move {
            let builder = conn::auto::Builder::new(TokioExecutor::new());
            let connection =
                builder.serve_connection_with_upgrades(io, service);
            tokio::pin!(connection);

            // This loop manages the connection's lifecycle.
            loop {
                tokio::select! {
                    // Poll the connection itself. This branch completes
                    // when the client disconnects or an error occurs.
                    _ = &mut connection => {
                        break;
                    }
                    // If the cancellation token is triggered, initiate
                    // a graceful shutdown of the Hyper connection.
                    _ = cancel.cancelled() => {
                        connection.as_mut().graceful_shutdown();
                    }
                }
            }
            // Drop the shutdown handle for this connection, signaling
            // that one fewer outstanding connection is active.
            drop(shutdown);
        });
    }
}

/// Handles dispatching of standard HTTP requests.
pub(crate) mod dispatch;
/// Provides the main Hyper service and request-handling logic.
pub(crate) mod service;
