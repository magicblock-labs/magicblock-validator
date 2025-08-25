use error::RpcError;
use magicblock_config::RpcConfig;
use magicblock_core::link::DispatchEndpoints;
use processor::EventProcessor;
use server::{http::HttpServer, websocket::WebsocketServer};
use state::SharedState;
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
        let mut addr = config.socket_addr();
        // Start up an event processor task, which will handle forwarding of any validator
        // originating event to client subscribers, or use them to update server's caches
        //
        // NOTE: currently we only start 1 instance, but it
        // can be scaled to more if that becomes a bottleneck
        EventProcessor::start(&state, dispatch, 1, cancel.clone());
        // bind http server at specified socket address
        let http = HttpServer::new(
            config.socket_addr(),
            &state,
            cancel.clone(),
            dispatch,
        )
        .await?;
        // for websocket use the same address but with port bumped by one
        addr.set_port(config.port + 1);
        let websocket = WebsocketServer::new(addr, &state, cancel).await?;
        Ok(Self { http, websocket })
    }

    /// Run JSON-RPC server indefinetely, until cancel token is used to signal shut down
    pub async fn run(self) {
        tokio::select! {
            _ = self.http.run() => {}
            _ = self.websocket.run() => {}
        }
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
