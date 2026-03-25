use error::{ApertureError, RpcError};
use magicblock_config::config::aperture::ApertureConfig;
use magicblock_core::link::DispatchEndpoints;
use processor::EventProcessor;
use server::{http::HttpServer, websocket::WebsocketServer};
use state::SharedState;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument};

type RpcResult<T> = Result<T, RpcError>;
type ApertureResult<T> = Result<T, ApertureError>;

pub async fn initialize_aperture(
    config: &ApertureConfig,
    state: SharedState,
    dispatch: &DispatchEndpoints,
    cancel: CancellationToken,
) -> ApertureResult<JsonRpcServer> {
    let server =
        JsonRpcServer::new(config, state.clone(), dispatch, cancel.clone())
            .await?;
    // Start event processors only after the server has bound its sockets so a
    // bind failure cannot leak background tasks during retries in tests/startup.
    EventProcessor::start(config, &state, dispatch, cancel)?;
    Ok(server)
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
        dispatch: &DispatchEndpoints,
        cancel: CancellationToken,
    ) -> ApertureResult<Self> {
        // try to bind to socket before spawning anything (handy in tests)
        let http = TcpListener::bind(config.listen.0)
            .await
            .map_err(RpcError::internal)?;
        let http_addr = http.local_addr().map_err(RpcError::internal)?;

        let mut ws_addr = http_addr;
        if config.listen.0.port() == 0 {
            ws_addr.set_port(0);
        } else {
            ws_addr.set_port(config.listen.0.port() + 1);
        }
        let ws = TcpListener::bind(ws_addr)
            .await
            .map_err(RpcError::internal)?;
        let ws_addr = ws.local_addr().map_err(RpcError::internal)?;

        // initialize HTTP and Websocket servers
        let websocket = {
            let cancel = cancel.clone();
            WebsocketServer::new(ws, &state, cancel).await?
        };
        let http = HttpServer::new(http, state, cancel, dispatch).await?;
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
pub mod error;
mod geyser;
mod processor;
mod requests;
pub mod server;
pub mod state;
#[cfg(test)]
mod tests;
mod utils;
