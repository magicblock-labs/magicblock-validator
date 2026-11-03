use std::sync::Arc;

use crate::requests::http::RpcHandlers;
use hyper::service::service_fn;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn,
};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

pub(crate) struct HttpServer {
    socket: TcpListener,
    dispatcher: Arc<RpcHandlers>,
    cancel: CancellationToken,
}

impl HttpServer {
    pub(crate) fn new(
        socket: TcpListener,
        dispatcher: Arc<RpcHandlers>,
        cancel: CancellationToken,
    ) -> Self {
        Self { socket, dispatcher, cancel }
    }

    pub(crate) async fn run(self) {
        loop {
            tokio::select! {
                biased;
                Ok((stream, _)) = self.socket.accept() => self.handle(stream),
                _ = self.cancel.cancelled() => break,
            }
        }
    }

    fn handle(&self, stream: TcpStream) {
        let cancel = self.cancel.child_token();
        let io = TokioIo::new(stream);
        let dispatcher = self.dispatcher.clone();
        let handler = service_fn(move |request| dispatch::dispatch(dispatcher.clone(), request));

        tokio::spawn(async move {
            let builder = conn::auto::Builder::new(TokioExecutor::new());
            let connection = builder.serve_connection(io, handler);
            tokio::pin!(connection);
            tokio::select! {
                _ = connection => {},
                _ = cancel.cancelled() => {},
            }
        });
    }
}

pub(crate) mod dispatch;
