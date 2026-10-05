#![doc = include_str!("../README.md")]

use account::Accounts;
use engine::Engine;
use history::History;
use magicblock_chainlink::ProdChainlink;
use magicblock_ledger_deprecated::Ledger;
use requests::http::RpcHandlers;
use std::{io, net::SocketAddr, sync::Arc, time::Duration};
use tokio::task::JoinSet;

pub use error::ApertureError;
use error::RpcError;
use magicblock_config::config::aperture::ApertureConfig;
use server::{http::HttpServer, websocket::WebsocketServer};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::info;

type RpcResult<T> = Result<T, RpcError>;
type ApertureResult<T> = Result<T, ApertureError>;

/// Bound HTTP/PubSub service whose consuming run owns background delivery tasks.
pub struct Aperture {
    http: HttpServer,
    websocket: WebsocketServer,
    http_addr: SocketAddr,
    ws_addr: SocketAddr,
    handlers: Arc<RpcHandlers>,
    geyser: Option<geyser::Delivery>,
    cancel: CancellationToken,
}

impl Aperture {
    /// Bind both listeners and prepare delivery without starting background tasks.
    pub async fn bind(
        config: &ApertureConfig,
        engine: Engine,
        chainlink: Arc<ProdChainlink>,
        legacy: Arc<Ledger>,
        blocktime: Duration,
        cancel: CancellationToken,
    ) -> ApertureResult<Self> {
        // Service exit cancels its connections without cancelling the caller's token.
        let cancel = cancel.child_token();
        // try to bind to socket before spawning anything (handy in tests)
        let http = TcpListener::bind(config.listen.0).await?;
        let http_addr = http.local_addr()?;

        let mut ws_addr = http_addr;
        let listen_port = config.listen.0.port();
        if listen_port == 0 {
            // Let OS assign random port for WS
            ws_addr.set_port(0);
        } else {
            let ws_port = listen_port.checked_add(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "RPC listen port {listen_port} leaves no room for the \
                     derived WebSocket port (listen + 1)."
                    ),
                )
            })?;
            ws_addr.set_port(ws_port);
        }
        let ws = TcpListener::bind(ws_addr).await?;
        let ws_addr = ws.local_addr()?;

        // Initialize HTTP and Websocket servers before starting any background
        // delivery tasks, so a bind failure cannot leak engine subscriptions.
        let websocket = WebsocketServer::new(ws, engine.clone(), cancel.clone());
        let accounts = Accounts::new(engine.clone(), chainlink);
        let history = History::new(engine.clone(), legacy);
        let handlers = RpcHandlers::new(engine.clone(), accounts, history, blocktime);
        let http = HttpServer::new(http, handlers.clone(), cancel.clone());
        let geyser = geyser::prepare(&config.geyser_plugins, engine)?;
        Ok(Self {
            http,
            websocket,
            http_addr,
            ws_addr,
            handlers,
            geyser,
            cancel,
        })
    }

    pub fn http_addr(&self) -> SocketAddr {
        self.http_addr
    }

    pub fn ws_addr(&self) -> SocketAddr {
        self.ws_addr
    }

    /// Runs until cancellation or any service task exits, then aborts and drains background tasks.
    pub async fn run(self) {
        // Also cancel detached connection tasks if the run future itself is dropped.
        let _cancel_on_drop = self.cancel.clone().drop_guard();
        info!("JSON-RPC server running");
        let mut tasks = JoinSet::new();
        tasks.spawn(self.handlers.run_perf_samples_collector(self.cancel.clone()));
        if let Some(geyser) = self.geyser {
            geyser.spawn(&mut tasks, self.cancel.clone());
        }
        tokio::select! {
            _ = self.http.run() => {},
            _ = self.websocket.run() => {},
            _ = self.cancel.cancelled() => {},
            _ = tasks.join_next() => {},
        }
        self.cancel.cancel();
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        info!("JSON-RPC server shutdown");
    }
}

mod account;
mod encoder;
mod error;
mod geyser;
mod history;
mod requests;
mod server;
mod state;
mod transaction;
