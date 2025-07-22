use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use fastwebsockets::{
    upgrade::upgrade, CloseCode, Frame, Payload, WebSocket, WebSocketError,
};
use http_body_util::Empty;
use hyper::{
    body::{Bytes, Incoming},
    server::conn::http1,
    service::service_fn,
    upgrade::Upgraded,
    Request, Response,
};
use hyper_util::rt::TokioIo;
use json::Value;
use log::warn;
use tokio::{
    net::{TcpListener, TcpStream},
    time::interval,
};
use tokio_util::sync::CancellationToken;

use crate::{
    error::RpcError,
    requests::{websocket::utils::SubResult, JsonRequest},
    state::SharedState,
    RpcResult,
};

use super::Shutdown;

type WebscoketStream = WebSocket<TokioIo<Upgraded>>;

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
    tokio::spawn(async move {
        let Ok(ws) = ws.await else {
            warn!("failed http upgrade to ws connection");
            return;
        };
        let handler = ConnectionHandler::new(ws, state, cancel, sd);
        handler.run().await
    });
    Ok(response)
}

struct ConnectionHandler {
    state: SharedState,
    cancel: CancellationToken,
    ws: WebscoketStream,
    _sd: Arc<Shutdown>,
}

impl ConnectionHandler {
    fn new(
        ws: WebscoketStream,
        state: SharedState,
        cancel: CancellationToken,
        _sd: Arc<Shutdown>,
    ) -> Self {
        Self {
            state,
            cancel,
            ws,
            _sd,
        }
    }

    async fn run(mut self) {
        const MAX_INACTIVE_INTERVAL: Duration = Duration::from_secs(60);
        let last_activity = Instant::now();
        let mut ping = interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                Ok(frame) = self.ws.read_frame() => {
                    let parsed = json::from_slice::<JsonRequest>(&frame.payload).map_err(RpcError::parse_error);
                    let request = match parsed {
                        Ok(r) => r,
                        Err(error) => {
                            self.report_failure(error, None).await;
                            continue;
                        }
                    };
                }
                _ = ping.tick() => {
                    if last_activity.elapsed() > MAX_INACTIVE_INTERVAL {
                        let frame = Frame::close(CloseCode::Policy.into(), b"connection inactive for too long");
                        let _ = self.ws.write_frame(frame).await;
                        break;
                    }
                }
                _ = self.cancel.cancelled() => break,
            }
        }
    }

    async fn report_success(
        &mut self,
        id: Value,
        result: SubResult,
    ) -> Result<(), WebSocketError> {
        let msg = json::json! {{
            "jsonrpc": "2.0",
            "result": result,
            "id": id
        }};
        let payload = json::to_vec(&msg)
            .expect("vec serialization for Value is infallible");
        self.send(payload).await
    }

    async fn report_failure(
        &mut self,
        error: RpcError,
        id: Option<Value>,
    ) -> Result<(), WebSocketError> {
        let msg = json::json! {{
            "jsonrpc": "2.0",
            "error": error,
            "id": id,
        }};
        let payload = json::to_vec(&msg)
            .expect("vec serialization for Value is infallible");
        self.send(payload).await
    }

    #[inline]
    async fn send(
        &mut self,
        payload: impl Into<Payload<'_>>,
    ) -> Result<(), WebSocketError> {
        let frame = Frame::text(payload.into());
        self.ws.write_frame(frame).await
    }
}
