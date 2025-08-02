use std::{net::SocketAddr, sync::Arc};

use dispatch::HttpDispatcher;
use hyper::service::service_fn;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn,
};
use magicblock_gateway_types::RpcChannelEndpoints;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot::{error::RecvError, Receiver},
};
use tokio_util::sync::CancellationToken;

use crate::{error::RpcError, state::SharedState, RpcResult};

use super::Shutdown;

pub(crate) struct HttpServer {
    socket: TcpListener,
    dispatcher: Arc<HttpDispatcher>,
    cancel: CancellationToken,
    shutdown: Arc<Shutdown>,
    shutdown_rx: Receiver<()>,
}

impl HttpServer {
    pub(crate) async fn new(
        addr: SocketAddr,
        state: &SharedState,
        cancel: CancellationToken,
        channels: &RpcChannelEndpoints,
    ) -> RpcResult<Self> {
        let socket =
            TcpListener::bind(addr).await.map_err(RpcError::internal)?;
        let (shutdown, shutdown_rx) = Shutdown::new();

        Ok(Self {
            socket,
            dispatcher: HttpDispatcher::new(state, channels),
            cancel,
            shutdown,
            shutdown_rx,
        })
    }

    pub(crate) async fn run(mut self) {
        loop {
            tokio::select! {
                biased; Ok((stream, _)) = self.socket.accept() => self.handle(stream),
                _ = self.cancel.cancelled() => break,
            }
        }
        drop(self.shutdown);
        let _ = self.shutdown_rx.await;
    }

    fn handle(&mut self, stream: TcpStream) {
        let cancel = self.cancel.child_token();

        let io = TokioIo::new(stream);
        let dispatcher = self.dispatcher.clone();
        let handler =
            service_fn(move |request| dispatcher.clone().dispatch(request));
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

pub(crate) mod dispatch;
