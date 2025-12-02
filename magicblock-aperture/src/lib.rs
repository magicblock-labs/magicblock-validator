use error::RpcError;
use log::*;
use magicblock_config::types::BindAddress;
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
        address: BindAddress,
        state: SharedState,
        dispatch: &DispatchEndpoints,
        cancel: CancellationToken,
    ) -> RpcResult<Self> {
        // try to bind to socket before spawning anything (handy in tests)
        let mut addr = address.0;
        let http = TcpListener::bind(addr).await.map_err(RpcError::internal)?;
        addr.set_port(addr.port() + 1);
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
        info!("Running JSON-RPC server");
        tokio::join! {
            self.http.run(),
            self.websocket.run()
        };
        info!("JSON-RPC server has shutdown");
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
