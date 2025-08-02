use std::sync::Arc;

use connection::ConnectionHandler;
use fastwebsockets::upgrade::upgrade;
use http_body_util::Empty;
use hyper::{
    body::{Bytes, Incoming},
    server::conn::http1,
    service::service_fn,
    Request, Response,
};
use hyper_util::rt::TokioIo;
use log::warn;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::{
    error::RpcError,
    requests::JsonRequest,
    state::{subscriptions::SubscriptionsDb, transactions::TransactionsCache},
    RpcResult,
};

use super::Shutdown;

pub struct WebsocketServer {
    socket: TcpListener,
    state: ConnectionState,
}

#[derive(Clone)]
struct ConnectionState {
    subscriptions: SubscriptionsDb,
    transactions: TransactionsCache,
    cancel: CancellationToken,
    shutdown: Arc<Shutdown>,
}

impl WebsocketServer {
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                Ok((stream, _)) = self.socket.accept() => {
                    self.handle(stream);
                },
                _ = self.state.cancel.cancelled() => break,
            }
        }
        self.state.shutdown.0.notified().await;
    }

    fn handle(&mut self, stream: TcpStream) {
        let state = self.state.clone();

        let io = TokioIo::new(stream);
        let handler =
            service_fn(move |request| handle_upgrade(request, state.clone()));

        tokio::spawn(async move {
            let builder = http1::Builder::new();
            let connection =
                builder.serve_connection(io, handler).with_upgrades();
            if let Err(error) = connection.await {
                warn!("websocket connection terminated with error: {error}");
            }
        });
    }
}

async fn handle_upgrade(
    request: Request<Incoming>,
    state: ConnectionState,
) -> RpcResult<Response<Empty<Bytes>>> {
    let (response, ws) = upgrade(request).map_err(RpcError::internal)?;
    tokio::spawn(async move {
        let Ok(ws) = ws.await else {
            warn!("failed http upgrade to ws connection");
            return;
        };
        let handler = ConnectionHandler::new(ws, state);
        handler.run().await
    });
    Ok(response)
}

pub(crate) mod connection;
pub(crate) mod dispatch;
