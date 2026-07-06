use std::collections::HashMap;

use engine::Engine;
use hyper::body::Bytes;
use json::{Serialize, Value};
use magicblock_metrics::metrics::RPC_REQUESTS_COUNT;
use tokio::{sync::mpsc, task::JoinHandle};

use super::connection::ConnectionID;
use crate::{
    RpcResult,
    error::RpcError,
    parse_params,
    requests::{JsonRpcWsMethod, JsonWsRequest},
    state::subscriptions::SubscriptionID,
};

/// The sender half of an MPSC channel used to push subscription notifications
/// to a single WebSocket client.
pub(crate) type ConnectionTx = mpsc::Sender<Bytes>;

/// The stateful request dispatcher for a single WebSocket connection.
///
/// An instance is created per connected client. Each subscribe request spawns a
/// dedicated Tokio task that subscribes to the relevant engine update stream,
/// encodes each update, and forwards it to this connection's channel. The task's
/// [`JoinHandle`] is retained keyed by the public subscription id so an
/// unsubscribe (or a dropped connection) can abort it.
pub(crate) struct WsDispatcher {
    /// The engine, used to open update subscriptions.
    pub(crate) engine: Engine,
    /// Forwarding tasks for this connection's active subscriptions, keyed by the
    /// public `SubscriptionID` returned to the client.
    pub(crate) unsubs: HashMap<SubscriptionID, JoinHandle<()>>,
    /// The communication channel for this specific connection.
    pub(crate) chan: WsConnectionChannel,
}

impl WsDispatcher {
    /// Creates a new dispatcher for a single client connection.
    pub(crate) fn new(engine: Engine, chan: WsConnectionChannel) -> Self {
        Self {
            engine,
            unsubs: Default::default(),
            chan,
        }
    }

    /// Returns the unique ID for this connection.
    pub(crate) fn connection_id(&self) -> ConnectionID {
        self.chan.id
    }

    /// Routes an incoming JSON-RPC request to the appropriate subscription handler.
    pub(crate) async fn dispatch(
        &mut self,
        request: &mut JsonWsRequest,
    ) -> RpcResult<WsDispatchResult> {
        use JsonRpcWsMethod::*;
        RPC_REQUESTS_COUNT
            .with_label_values(&[request.method.as_str()])
            .inc();
        let result = match request.method {
            AccountSubscribe => self.account_subscribe(request).await,
            ProgramSubscribe => self.program_subscribe(request).await,
            SignatureSubscribe => self.signature_subscribe(request).await,
            SlotSubscribe => self.slot_subscribe().await,
            LogsSubscribe => self.logs_subscribe(request).await,
            AccountUnsubscribe | ProgramUnsubscribe | LogsUnsubscribe
            | SlotUnsubscribe | SignatureUnsubscribe => {
                self.unsubscribe(request)
            }
            Ping => Ok(SubResult::Pong("pong")),
        }?;

        Ok(WsDispatchResult {
            id: request.id.take(),
            result,
        })
    }

    /// Handles a request to unsubscribe from a previously established subscription.
    ///
    /// Removes the subscription's forwarding task and aborts it, stopping further
    /// notifications for that id.
    fn unsubscribe(
        &mut self,
        request: &mut JsonWsRequest,
    ) -> RpcResult<SubResult> {
        let mut params = request
            .params
            .take()
            .ok_or_else(|| RpcError::invalid_request("missing params"))?;

        let id = parse_params!(params, SubscriptionID).ok_or_else(|| {
            RpcError::invalid_params("missing or invalid subscription id")
        })?;

        let success = self
            .unsubs
            .remove(&id)
            .inspect(|handle| handle.abort())
            .is_some();
        Ok(SubResult::Unsub(success))
    }

    /// Registers a spawned forwarding task under its subscription id. A duplicate
    /// id (should not happen with the global counter) aborts the previous task.
    pub(crate) fn register(
        &mut self,
        id: SubscriptionID,
        handle: JoinHandle<()>,
    ) {
        if let Some(previous) = self.unsubs.insert(id, handle) {
            previous.abort();
        }
    }
}

impl Drop for WsDispatcher {
    /// Aborts every forwarding task when the connection goes away.
    fn drop(&mut self) {
        for (_, handle) in self.unsubs.drain() {
            handle.abort();
        }
    }
}

/// Bundles a connection's unique ID with its dedicated sender channel.
#[derive(Clone)]
pub(crate) struct WsConnectionChannel {
    pub(crate) id: ConnectionID,
    pub(crate) tx: ConnectionTx,
}

/// An enum representing the successful result of a subscription or unsubscription request.
#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum SubResult {
    /// A new subscription ID.
    SubId(SubscriptionID),
    /// The result of an unsubscription request (`true` for success).
    Unsub(bool),
    /// The heartbeat response message
    Pong(&'static str),
}

/// A container for a successfully processed RPC request, pairing the result with
/// the original request ID for the client to correlate.
pub(crate) struct WsDispatchResult {
    pub(crate) id: Value,
    pub(crate) result: SubResult,
}
