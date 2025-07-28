use std::{net::SocketAddr, sync::Arc};

use hyper::service::service_fn;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn,
};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::{error::RpcError, requests, state::SharedState, RpcResult};

use super::Shutdown;

struct HttpServer {
    socket: TcpListener,
    state: SharedState,
    cancel: CancellationToken,
    shutdown: Arc<Shutdown>,
}

impl HttpServer {
    async fn new(
        addr: SocketAddr,
        state: SharedState,
        cancel: CancellationToken,
    ) -> RpcResult<Self> {
        let socket =
            TcpListener::bind(addr).await.map_err(RpcError::internal)?;
        let shutdown = Arc::default();
        Ok(Self {
            socket,
            state,
            cancel,
            shutdown,
        })
    }

    async fn run(mut self) {
        loop {
            tokio::select! {
                biased; Ok((stream, _)) = self.socket.accept() => self.handle(stream),
                _ = self.cancel.cancelled() => break,
            }
        }
        self.shutdown.0.notified().await;
    }

    fn handle(&mut self, stream: TcpStream) {
        let state = self.state.clone();
        let cancel = self.cancel.child_token();

        let io = TokioIo::new(stream);
        let handler = service_fn(move |request| {
            requests::http::dispatch(request, state.clone())
        });
        let shutdown = self.shutdown.clone();

        tokio::spawn(async move {
            let builder = conn::auto::Builder::new(TokioExecutor::new());
            let connection = builder.serve_connection(io, handler);
            tokio::pin!(connection);
            loop {
                tokio::select! {
                    _ = &mut connection => {
                        break;
                    }
                    _ = cancel.cancelled() => {
                        connection.as_mut().graceful_shutdown();
                    }
                }
            }
            drop(shutdown);
        });
    }
}
