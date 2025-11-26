use std::sync::Arc;

use dispatch::HttpDispatcher;
use hyper::service::service_fn;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn,
};
use magicblock_core::link::DispatchEndpoints;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot::Receiver,
};
use tokio_util::sync::CancellationToken;

use super::Shutdown;
use crate::{state::SharedState, RpcResult};

/// A graceful, Tokio-based HTTP server built with Hyper.
///
/// This server is responsible for accepting raw TCP connections and managing their
/// lifecycle. It uses a shared `HttpDispatcher` to process incoming requests and
/// supports graceful shutdown to ensure in-flight requests are completed before termination.
pub(crate) struct HttpServer {
    /// The TCP listener that accepts incoming connections.
    socket: TcpListener,
    /// The shared request handler that contains the application's RPC logic.
    dispatcher: Arc<HttpDispatcher>,
    /// The main cancellation token. When triggered, the server stops accepting new connections.
    cancel: CancellationToken,
    /// A shared RAII guard for tracking in-flight connections. When all clones of this
    /// `Arc` are dropped, the `shutdown_rx` receiver is notified.
    shutdown: Arc<Shutdown>,
    /// The receiving end of the shutdown signal, used to wait for all connections to terminate.
    shutdown_rx: Receiver<()>,
}

impl HttpServer {
    /// Initializes the HTTP server by binding to an address and setting up shutdown signaling.
    pub(crate) async fn new(
        socket: TcpListener,
        state: SharedState,
        cancel: CancellationToken,
        dispatch: &DispatchEndpoints,
    ) -> RpcResult<Self> {
        let (shutdown, shutdown_rx) = Shutdown::new();

        Ok(Self {
            socket,
            dispatcher: HttpDispatcher::new(state, dispatch),
            cancel,
            shutdown,
            shutdown_rx,
        })
    }

    /// Starts the main server loop, accepting connections until a shutdown signal is received.
    ///
    /// ## Graceful Shutdown
    ///
    /// The shutdown process occurs in two phases:
    /// 1.  When the `cancel` token is triggered, the server immediately stops accepting
    ///     new connections.
    /// 2.  The server then waits for all active connections (which hold a clone of the
    ///     `shutdown` handle) to complete their work and drop their handles. Only then
    ///     does the `run` method return.
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
    ///
    /// Each connection is managed by a Hyper connection handler and is integrated with
    /// the server's cancellation mechanism for graceful shutdown.
    fn handle(&self, stream: TcpStream) {
        // Create a child token so this specific connection can be cancelled.
        let cancel = self.cancel.child_token();

        let io = TokioIo::new(stream);
        let dispatcher = self.dispatcher.clone();
        let handler =
            service_fn(move |request| dispatcher.clone().dispatch(request));
        let shutdown = self.shutdown.clone();

        tokio::spawn(async move {
            let builder = conn::auto::Builder::new(TokioExecutor::new());
            let connection = builder.serve_connection(io, handler);
            tokio::pin!(connection);
            let mut terminating = false;

            // This loop manages the connection's lifecycle.
            loop {
                tokio::select! {
                    // Poll the connection itself. This branch
                    // completes when the client disconnects.
                    _ = &mut connection => {
                        break;
                    }
                    // If the cancellation token is triggered, initiate a graceful shutdown
                    // of the Hyper connection.
                    _ = cancel.cancelled(), if !terminating => {
                        connection.as_mut().graceful_shutdown();
                        terminating = true;
                    }
                }
            }
            // Drop the shutdown handle for this connection, signaling
            // that one fewer outstanding connection is active.
            drop(shutdown);
        });
    }
}

pub(crate) mod dispatch;
