use std::{
    sync::{atomic::AtomicU32, Arc},
    time::{Duration, Instant},
};

use fastwebsockets::{CloseCode, Frame, Payload, WebSocket, WebSocketError};
use hyper::{body::Bytes, upgrade::Upgraded};
use hyper_util::rt::TokioIo;
use log::debug;
use tokio::{
    sync::mpsc::{self, Receiver},
    time,
};
use tokio_util::sync::CancellationToken;

use crate::{
    error::RpcError,
    requests::JsonRequest,
    server::{websocket::dispatch::WsConnectionChannel, Shutdown},
    state::SharedState,
};

use super::dispatch::{WsDispatchResult, WsDispatcher};

type WebscoketStream = WebSocket<TokioIo<Upgraded>>;
pub(crate) type ConnectionID = u32;

pub(super) struct ConnectionHandler {
    cancel: CancellationToken,
    ws: WebscoketStream,
    dispatcher: WsDispatcher,
    updates_rx: Receiver<Bytes>,
    _sd: Arc<Shutdown>,
}

impl ConnectionHandler {
    pub(super) fn new(
        ws: WebscoketStream,
        state: SharedState,
        cancel: CancellationToken,
        _sd: Arc<Shutdown>,
    ) -> Self {
        static CONNECTION_COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = CONNECTION_COUNTER.load(std::sync::atomic::Ordering::Relaxed);
        let (tx, updates_rx) = mpsc::channel(4096);
        let chan = WsConnectionChannel { id, tx };
        let dispatcher = WsDispatcher::new(state, chan);
        Self {
            dispatcher,
            cancel,
            ws,
            updates_rx,
            _sd,
        }
    }

    pub(super) async fn run(mut self) {
        const MAX_INACTIVE_INTERVAL: Duration = Duration::from_secs(60);
        let last_activity = Instant::now();
        let mut ping = time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                biased; Ok(frame) = self.ws.read_frame() => {
                    let parsed = json::from_slice::<JsonRequest>(&frame.payload)
                        .map_err(RpcError::parse_error);
                    let request = match parsed {
                        Ok(r) => r,
                        Err(error) => {
                            self.report_failure(error).await;
                            continue;
                        }
                    };
                    let success = match self.dispatcher.dispatch(request).await {
                        Ok(r) => self.report_success(r).await,
                        Err(e) => self.report_failure(e).await,
                    };
                    if !success { break };
                }
                _ = ping.tick() => {
                    if last_activity.elapsed() > MAX_INACTIVE_INTERVAL {
                        let frame = Frame::close(CloseCode::Policy.into(), b"connection inactive for too long");
                        let _ = self.ws.write_frame(frame).await;
                        break;
                    }
                }
                _ = self.cancel.cancelled() => break,
                _ = self.dispatcher.cleanup() => {}
            }
        }
    }

    async fn report_success(&mut self, result: WsDispatchResult) -> bool {
        let msg = json::json! {{
            "jsonrpc": "2.0",
            "result": result.result,
            "id": result.id
        }};
        let payload = json::to_vec(&msg)
            .expect("vec serialization for Value is infallible");
        self.send(payload).await.is_ok()
    }

    async fn report_failure(&mut self, error: RpcError) -> bool {
        let msg = json::json! {{
            "jsonrpc": "2.0",
            "error": error,
            "id": None::<()>,
        }};
        let payload = json::to_vec(&msg)
            .expect("vec serialization for Value is infallible");
        self.send(payload).await.is_ok()
    }

    #[inline]
    async fn send(
        &mut self,
        payload: impl Into<Payload<'_>>,
    ) -> Result<(), WebSocketError> {
        let frame = Frame::text(payload.into());
        self.ws.write_frame(frame).await.inspect_err(|e| {
            debug!("failed to send websocket frame to the client: {e}")
        })
    }
}
