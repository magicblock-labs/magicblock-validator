use std::net::SocketAddr;

use error::{ApertureError, RpcError};
use magicblock_config::config::aperture::ApertureConfig;
use server::{http::HttpServer, websocket::WebsocketServer};
use state::SharedState;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument};

type RpcResult<T> = Result<T, RpcError>;
type ApertureResult<T> = Result<T, ApertureError>;

pub async fn initialize_aperture(
    config: &ApertureConfig,
    state: SharedState,
    cancel: CancellationToken,
) -> ApertureResult<JsonRpcServer> {
    // Reads, subscriptions and transaction submission are all served directly
    // from the engine held in `state`; there is no separate event-processing
    // stage to start here.
    JsonRpcServer::new(config, state, cancel).await
}

/// An entrypoint to startup JSON-RPC server, for both HTTP and WS requests
pub struct JsonRpcServer {
    http: HttpServer,
    websocket: WebsocketServer,
    http_addr: SocketAddr,
    ws_addr: SocketAddr,
}

impl JsonRpcServer {
    /// Create a new instance of JSON-RPC server, hooked into validator via dispatch channels
    async fn new(
        config: &ApertureConfig,
        state: SharedState,
        cancel: CancellationToken,
    ) -> ApertureResult<Self> {
        // try to bind to socket before spawning anything (handy in tests)
        let http = TcpListener::bind(config.listen.0)
            .await
            .map_err(RpcError::internal)?;
        let http_addr = http.local_addr().map_err(RpcError::internal)?;

        let mut ws_addr = http_addr;
        let listen_port = config.listen.0.port();
        if listen_port == 0 {
            // Let OS assign random port for WS
            ws_addr.set_port(0);
        } else {
            let ws_port = listen_port.checked_add(1).ok_or_else(|| {
                RpcError::internal(format!(
                    "RPC listen port {listen_port} leaves no room for the \
                     derived WebSocket port (listen + 1)."
                ))
            })?;
            ws_addr.set_port(ws_port);
        }
        let ws = TcpListener::bind(ws_addr)
            .await
            .map_err(RpcError::internal)?;
        let ws_addr = ws.local_addr().map_err(RpcError::internal)?;

        // Initialize HTTP and Websocket servers before starting any background
        // delivery tasks, so a bind failure cannot leak engine subscriptions.
        let websocket = {
            let cancel = cancel.clone();
            WebsocketServer::new(ws, &state, cancel).await?
        };
        let http = HttpServer::new(http, state.clone(), cancel.clone()).await?;
        let _geyser = geyser::start(
            &config.geyser_plugins,
            config.event_processors,
            state.engine,
            cancel,
        );
        Ok(Self {
            http,
            websocket,
            http_addr,
            ws_addr,
        })
    }

    pub fn http_addr(&self) -> SocketAddr {
        self.http_addr
    }

    pub fn ws_addr(&self) -> SocketAddr {
        self.ws_addr
    }

    /// Run JSON-RPC server indefinitely, until cancel token is used to signal shut down
    #[instrument(skip(self))]
    pub async fn run(self) {
        info!("JSON-RPC server running");
        tokio::join! {
            self.http.run(),
            self.websocket.run()
        };
        info!("JSON-RPC server shutdown");
    }
}

mod encoder;
mod engine_types;
pub mod error;
mod geyser;
mod requests;
pub mod server;
pub mod state;
mod utils;
