use std::sync::Arc;

use fastwebsockets::upgrade::{upgrade, UpgradeFut};
use http_body_util::Empty;
use hyper::{
    body::{Bytes, Incoming},
    server::conn::http1,
    service::service_fn,
    Request, Response,
};
use hyper_util::rt::TokioIo;
use log::warn;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Notify,
};
use tokio_util::sync::CancellationToken;

use crate::{error::RpcError, state::SharedState, RpcResult};

pub struct WebsocketServer {
    socket: TcpListener,
    state: SharedState,
    cancel: CancellationToken,
    shutdown: Arc<Shutdown>,
}

impl WebsocketServer {
    async fn run(mut self) {
        loop {
            tokio::select! {
                Ok((stream, _)) = self.socket.accept() => {
                    self.handle(stream);
                },
                _ = self.cancel.cancelled() => break,
            }
        }
        self.shutdown.0.notified().await;
    }

    fn handle(&mut self, stream: TcpStream) {
        let state = self.state.clone();
        let cancel = self.cancel.child_token();
        let sd = self.shutdown.clone();

        let io = TokioIo::new(stream);
        let handler = service_fn(move |request| {
            handle_upgrade(request, state.clone(), cancel.clone(), sd.clone())
        });

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
    state: SharedState,
    cancel: CancellationToken,
    sd: Arc<Shutdown>,
) -> RpcResult<Response<Empty<Bytes>>> {
    let (response, ws) = upgrade(request).map_err(RpcError::internal)?;
    tokio::spawn(handle_ws_connection(
        ws,
        state.clone(),
        cancel.clone(),
        sd.clone(),
    ));
    Ok(response)
}

async fn handle_ws_connection(
    ws: UpgradeFut,
    state: SharedState,
    cancel: CancellationToken,
    _sd: Arc<Shutdown>,
) {
    let Ok(ws) = ws.await else {
        warn!("failed to upgrade to ws connection");
        return;
    };
    todo!()
}

#[derive(Default)]
struct Shutdown(Notify);
impl Drop for Shutdown {
    fn drop(&mut self) {
        self.0.notify_last();
    }
}
