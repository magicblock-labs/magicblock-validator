use std::collections::HashMap;

use engine::Engine;
use hyper::body::Bytes;
use json::Serialize;
use magicblock_metrics::metrics::RPC_REQUESTS_COUNT;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::warn;

use crate::{
    RpcResult,
    error::RpcError,
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
    pub(crate) tx: ConnectionTx,
}

impl WsDispatcher {
    /// Creates a new dispatcher for a single client connection.
    pub(crate) fn new(engine: Engine, tx: ConnectionTx) -> Self {
        Self {
            engine,
            unsubs: Default::default(),
            tx,
        }
    }

    /// Routes an incoming JSON-RPC request to the appropriate subscription handler.
    pub(crate) async fn dispatch(&mut self, request: &JsonWsRequest) -> RpcResult<SubResult> {
        use JsonRpcWsMethod::*;
        RPC_REQUESTS_COUNT.with_label_values(&[request.method.as_str()]).inc();
        match request.method {
            AccountSubscribe => self.account_subscribe(request),
            ProgramSubscribe => self.program_subscribe(request),
            SignatureSubscribe => self.signature_subscribe(request).await,
            SlotSubscribe => self.slot_subscribe(),
            LogsSubscribe => self.logs_subscribe(request).await,
            AccountUnsubscribe | ProgramUnsubscribe | LogsUnsubscribe | SlotUnsubscribe
            | SignatureUnsubscribe => self.unsubscribe(request),
            Ping => Ok(SubResult::Pong("pong")),
            MethodNotFound => Err(RpcError::method_not_found()),
        }
    }

    /// Handles a request to unsubscribe from a previously established subscription.
    ///
    /// Removes the subscription's forwarding task and aborts it, stopping further
    /// notifications for that id.
    fn unsubscribe(&mut self, request: &JsonWsRequest) -> RpcResult<SubResult> {
        let id = request.required::<SubscriptionID>(0)?;

        let success = self.unsubs.remove(&id).inspect(|handle| handle.abort()).is_some();
        Ok(SubResult::Unsub(success))
    }

    /// Forward every streaming subscription through the same delivery loop.
    pub(crate) fn forward<T: Send + 'static>(
        &mut self,
        id: SubscriptionID,
        mut rx: mpsc::Receiver<T>,
        encode: impl Fn(T) -> RpcResult<Option<Bytes>> + Send + 'static,
    ) {
        let tx = self.tx.clone();
        let handle = tokio::spawn(async move {
            while let Some(update) = rx.recv().await {
                let bytes = match encode(update) {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => continue,
                    Err(error) => {
                        warn!(?error, id, "subscription serialization failed");
                        break;
                    }
                };
                if tx.send(bytes).await.is_err() {
                    break;
                }
            }
        });
        self.register(id, handle);
    }

    /// Registers a spawned forwarding task under its subscription id. A duplicate
    /// id (should not happen with the global counter) aborts the previous task.
    pub(crate) fn register(&mut self, id: SubscriptionID, handle: JoinHandle<()>) {
        // Completed one-shot subscriptions no longer need connection-owned handles.
        self.unsubs.retain(|_, task| !task.is_finished());
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
