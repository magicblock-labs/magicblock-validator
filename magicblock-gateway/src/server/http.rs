use std::net::SocketAddr;

use futures::{stream::FuturesUnordered, StreamExt};
use hyper::service::service_fn;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn,
};
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{error::RpcError, requests, state::SharedState, RpcResult};

struct HttpServer {
    socket: TcpListener,
    state: SharedState,
    cancel: CancellationToken,
    connections: FuturesUnordered<JoinHandle<()>>,
}

impl HttpServer {
    async fn new(
        addr: SocketAddr,
        state: SharedState,
        cancel: CancellationToken,
    ) -> RpcResult<Self> {
        let socket =
            TcpListener::bind(addr).await.map_err(RpcError::internal)?;
        let connections = FuturesUnordered::new();
        Ok(Self {
            socket,
            state,
            connections,
            cancel,
        })
    }

    async fn run(mut self) {
        loop {
            tokio::select! {
                Ok((stream, _)) = self.socket.accept() => self.handle(stream),
                _ = self.cancel.cancelled() => break,
                _ = self.connections.next(), if !self.connections.is_empty() => {}
            }
        }
        while self.connections.next().await.is_some() {}
    }

    fn handle(&mut self, stream: TcpStream) {
        let state = self.state.clone();
        let cancel = self.cancel.child_token();

        let io = TokioIo::new(stream);
        let handler = service_fn(move |request| {
            requests::http::dispatch(request, state.clone())
        });

        let handle = tokio::spawn(async move {
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
        });
        self.connections.push(handle);
    }
}
