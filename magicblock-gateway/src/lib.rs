use error::RpcError;
use magicblock_config::RpcConfig;
use server::{http::HttpServer, websocket::WebsocketServer};
use state::SharedState;
use tokio_util::sync::CancellationToken;
use types::RpcChannelEndpoints;

mod encoder;
pub mod error;
mod processor;
mod requests;
pub mod server;
pub mod state;
pub mod types;
mod utils;

type RpcResult<T> = Result<T, RpcError>;
type Slot = u64;

pub struct JsonRpcServer {
    http: HttpServer,
    websocket: WebsocketServer,
}

impl JsonRpcServer {
    pub async fn new(
        config: RpcConfig,
        state: SharedState,
        channels: RpcChannelEndpoints,
        cancel: CancellationToken,
    ) -> RpcResult<Self> {
        let mut addr = config.socket_addr();
        let http = HttpServer::new(
            config.socket_addr(),
            &state,
            cancel.clone(),
            &channels,
        )
        .await?;
        addr.set_port(config.port + 1);
        let websocket = WebsocketServer::new(addr, &state, cancel).await?;
        Ok(Self { http, websocket })
    }

    pub async fn run(self) {
        tokio::select! {
            _ = self.http.run() => {}
            _ = self.websocket.run() => {}
        }
    }
}
