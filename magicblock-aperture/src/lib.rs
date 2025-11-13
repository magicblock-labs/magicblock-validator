use error::{ApertureError, RpcError};
use magicblock_config::ApertureConfig;
use magicblock_core::link::DispatchEndpoints;
use processor::EventProcessor;
use server::{http::HttpServer, websocket::WebsocketServer};
use state::SharedState;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

type RpcResult<T> = Result<T, RpcError>;
type ApertureResult<T> = Result<T, ApertureError>;

pub async fn initialize_aperture(
    config: &ApertureConfig,
    state: SharedState,
    dispatch: &DispatchEndpoints,
    cancel: CancellationToken,
) -> ApertureResult<JsonRpcServer> {
    // Start up an event processor tasks, which will handle forwarding of any validator
    // originating event to client subscribers, or use them to update server's caches
    EventProcessor::start(
        &state,
        dispatch,
        config.event_processors,
        cancel.clone(),
        &config.geyser_plugins,
    )?;
    let server = JsonRpcServer::new(config, state, dispatch, cancel).await?;
    Ok(server)
}

/// An entrypoint to startup JSON-RPC server, for both HTTP and WS requests
pub struct JsonRpcServer {
    http: HttpServer,
    websocket: WebsocketServer,
}

impl JsonRpcServer {
    /// Create a new instance of JSON-RPC server, hooked into validator via dispatch channels
    pub async fn new(
        config: &ApertureConfig,
        state: SharedState,
        dispatch: &DispatchEndpoints,
        cancel: CancellationToken,
    ) -> ApertureResult<Self> {
        // try to bind to socket before spawning anything (handy in tests)
        let mut addr = config.socket_addr();
        let http = TcpListener::bind(addr).await.map_err(RpcError::internal)?;
        addr.set_port(config.port + 1);
        let ws = TcpListener::bind(addr).await.map_err(RpcError::internal)?;

        // initialize HTTP and Websocket servers
        let websocket = {
            let cancel = cancel.clone();
            WebsocketServer::new(ws, &state, cancel).await?
        };
        let http = HttpServer::new(http, state, cancel, dispatch).await?;
        Ok(Self { http, websocket })
    }

    /// Run JSON-RPC server indefinitely, until cancel token is used to signal shut down
    pub async fn run(self) {
        tokio::join! {
            self.http.run(),
            self.websocket.run()
        };
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
