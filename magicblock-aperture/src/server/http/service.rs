use std::sync::Arc;

use fastwebsockets::upgrade::upgrade;
use hyper::{body::Incoming, Request, Response};
use tokio::sync::mpsc::Sender;

use crate::{
    error::RpcError, server::websocket::is_websocket_upgrade, utils::JsonBody,
    RpcResult,
};

use super::{HttpDispatcher, HttpServer, WebsocketStream};

/// A clonable service that routes requests to HTTP or WebSocket handlers.
///
/// This struct acts as the primary entry point for a Hyper connection. It
/// inspects request headers to determine the correct protocol and delegates
/// handling accordingly.
#[derive(Clone)]
pub struct RequestHandler {
    /// Dispatches standard JSON-RPC over HTTP.
    http: Arc<HttpDispatcher>,
    /// Sends established WebSocket streams to the dedicated `WebsocketServer`.
    websocket: Sender<WebsocketStream>,
}

impl RequestHandler {
    /// Creates a new `RequestHandler` from shared server components.
    pub(crate) fn new(server: &HttpServer) -> Self {
        Self {
            http: server.dispatcher.clone(),
            websocket: server.websocket.clone(),
        }
    }

    /// Handles an incoming request, dispatching it based on its type.
    pub(crate) async fn handle(
        self,
        request: Request<Incoming>,
    ) -> RpcResult<Response<JsonBody>> {
        // Discriminate between a WebSocket upgrade and a standard HTTP request.
        if !is_websocket_upgrade(&request) {
            // Delegate to the HTTP dispatcher for standard JSON-RPC.
            return self
                .http
                .dispatch(request)
                .await
                .map_err(RpcError::internal);
        }
        // Otherwise perform the WebSocket upgrade handshake.
        let (response, ws_fut) =
            upgrade(request).map_err(RpcError::internal)?;

        // On success, await the future and send the stream to the handler.
        tokio::spawn(async move {
            let ws = ws_fut.await.unwrap();
            let _ = self.websocket.send(ws).await;
        });

        // Return the "101 Switching Protocols" response.
        Ok(response.map(|_| JsonBody(vec![])))
    }
}
