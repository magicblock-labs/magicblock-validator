use std::sync::Arc;

use dispatch::HttpDispatcher;
use hyper::service::service_fn;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn,
};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::{RpcResult, state::SharedState};

pub(crate) struct HttpServer {
    socket: TcpListener,
    dispatcher: Arc<HttpDispatcher>,
    cancel: CancellationToken,
}

impl HttpServer {
    pub(crate) async fn new(
        socket: TcpListener,
        state: SharedState,
        cancel: CancellationToken,
    ) -> RpcResult<Self> {
        Ok(Self {
            socket,
            dispatcher: HttpDispatcher::new(state),
            cancel,
        })
    }

    pub(crate) async fn run(self) {
        let dispatcher = self.dispatcher.clone();
        tokio::spawn(
            dispatcher.run_perf_samples_collector(self.cancel.clone()),
        );
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
        let handler =
            service_fn(move |request| dispatcher.clone().dispatch(request));

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
