use error::RpcError;
use magicblock_config::RpcConfig;
use magicblock_core::link::DispatchEndpoints;
use processor::EventProcessor;
use server::{http::HttpServer, websocket::WebsocketServer};
use state::SharedState;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

type RpcResult<T> = Result<T, RpcError>;

/// An entrypoint to startup JSON-RPC server, for both HTTP and WS requests
pub struct JsonRpcServer {
    http: HttpServer,
    websocket: WebsocketServer,
}

impl JsonRpcServer {
    /// Create a new instance of JSON-RPC server, hooked into validator via dispatch channels
    pub async fn new(
        config: &RpcConfig,
        state: SharedState,
        dispatch: &DispatchEndpoints,
        cancel: CancellationToken,
    ) -> RpcResult<Self> {
        // try to bind to socket before spawning anything (handy in tests)
        let mut addr = config.socket_addr();
        let http = TcpListener::bind(addr).await.map_err(RpcError::internal)?;
        addr.set_port(config.port + 1);
        let ws = TcpListener::bind(addr).await.map_err(RpcError::internal)?;

        // Start up an event processor task, which will handle forwarding of any validator
        // originating event to client subscribers, or use them to update server's caches
        //
        // NOTE: currently we only start 2 instances, but it
        // can be scaled to more if that becomes a bottleneck
        EventProcessor::start(&state, dispatch, 2, cancel.clone());

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
mod processor;
mod requests;
pub mod server;
pub mod state;
#[cfg(test)]
mod tests;
mod utils;
