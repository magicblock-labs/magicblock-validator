use std::sync::Arc;

use dispatch::HttpDispatcher;
use hyper::service::service_fn;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn,
};
use magicblock_core::link::DispatchEndpoints;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument};

use crate::{state::SharedState, RpcResult};

/// A Tokio-based HTTP server built with Hyper.
///
/// This server is responsible for accepting raw TCP connections and managing their
/// lifecycle. It uses a shared `HttpDispatcher` to process incoming requests.
pub(crate) struct HttpServer {
    /// The TCP listener that accepts incoming connections.
    socket: TcpListener,
    /// The shared request handler that contains the application's RPC logic.
    dispatcher: Arc<HttpDispatcher>,
    /// The main cancellation token. When triggered, the server stops accepting new connections.
    cancel: CancellationToken,
}

impl HttpServer {
    /// Initializes the HTTP server by binding to an address and setting up shutdown signaling.
    pub(crate) async fn new(
        socket: TcpListener,
        state: SharedState,
        cancel: CancellationToken,
        dispatch: &DispatchEndpoints,
    ) -> RpcResult<Self> {
        Ok(Self {
            socket,
            dispatcher: HttpDispatcher::new(state, dispatch),
            cancel,
        })
    }

    /// Starts the main server loop, accepting connections until a shutdown signal is received.
    ///
    /// When the `cancel` token is triggered, the server stops accepting new
    /// connections and returns immediately so validator restart time is not
    /// blocked by active HTTP requests.
    #[instrument(skip(self))]
    pub(crate) async fn run(self) {
        let dispatcher = self.dispatcher.clone();
        let cancel = self.cancel.clone();
        tokio::spawn(dispatcher.run_perf_samples_collector(cancel));
        loop {
            tokio::select! {
                biased;
                // Accept a new incoming connection.
                Ok((stream, _)) = self.socket.accept() => self.handle(stream),
                // Or, break the loop if the cancellation token is triggered.
                _ = self.cancel.cancelled() => {
                    info!("HTTP server shutdown signal received");
                    break
                }
            }
        }
        info!("HTTP server shutdown");
    }

    /// Spawns a new task to handle a single incoming TCP connection.
    ///
    /// Each connection is managed by a Hyper connection handler and is integrated with
    /// the server's cancellation mechanism.
    fn handle(&self, stream: TcpStream) {
        // Create a child token so this specific connection can be cancelled.
        let cancel = self.cancel.child_token();

        let io = TokioIo::new(stream);
        let dispatcher = self.dispatcher.clone();
        let handler =
            service_fn(move |request| dispatcher.clone().dispatch(request));

        tokio::spawn(async move {
            let builder = conn::auto::Builder::new(TokioExecutor::new());
            let connection = builder.serve_connection(io, handler);
            tokio::pin!(connection);
            // This loop manages the connection's lifecycle.
            tokio::select! {
                // Poll the connection itself. This branch
                // completes when the client disconnects.
                _ = connection => {},
                // If the cancellation token is triggered, force terminate the connection
                _ = cancel.cancelled() => {},
            }
        });
    }
}

pub(crate) mod dispatch;
