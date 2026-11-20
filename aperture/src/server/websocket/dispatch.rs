use std::{
    collections::HashMap,
    future::Future,
    sync::atomic::{AtomicU64, Ordering},
};

use ::metrics::metrics::RPC_REQUESTS_COUNT;
use engine::Engine;
use hyper::body::Bytes;
use serde::Serialize;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::{
    RpcResult,
    error::{RpcError, SUBSCRIPTION_LIMIT_REACHED},
    requests::{JsonRpcWsMethod, JsonWsRequest},
    state::subscriptions::SubscriptionID,
};

const MAX_SUBSCRIPTIONS: usize = 512;

/// The stateful request dispatcher for a single WebSocket connection.
pub(crate) struct WsDispatcher {
    /// The engine, used to open update subscriptions.
    pub(crate) engine: Engine,
    /// Forwarding task handles keyed by subscription ID, including finished
    /// tasks that have not yet been pruned.
    tasks: HashMap<SubscriptionID, JoinHandle<()>>,
    /// The communication channel for this specific connection.
    tx: mpsc::Sender<Bytes>,
    /// Cancels only this connection; server shutdown propagates through its parent.
    pub(super) cancel: CancellationToken,
}

impl WsDispatcher {
    /// Creates a new dispatcher for a single client connection.
    pub(crate) fn new(engine: Engine, tx: mpsc::Sender<Bytes>, cancel: CancellationToken) -> Self {
        Self {
            engine,
            tasks: HashMap::new(),
            tx,
            cancel,
        }
    }

    /// Routes an incoming JSON-RPC request to the appropriate subscription handler.
    pub(crate) async fn dispatch(&mut self, request: &JsonWsRequest) -> RpcResult<SubResult> {
        use JsonRpcWsMethod::*;
        RPC_REQUESTS_COUNT.with_label_values(&[request.method.as_str()]).inc();
        match request.method {
            AccountUnsubscribe
            | ProgramUnsubscribe
            | LogsUnsubscribe
            | SlotUnsubscribe
            | SignatureUnsubscribe
            | TransactionFailureUnsubscribe => self.unsubscribe(request),
            Ping => Ok(SubResult::Pong("pong")),
            MethodNotFound => Err(RpcError::method_not_found()),
            // Dispatch is serial per connection, so admission precedes any Engine registration.
            _ if !self.has_subscription_slot() => Err(RpcError::custom(
                format!("WebSocket subscription limit reached ({MAX_SUBSCRIPTIONS})"),
                SUBSCRIPTION_LIMIT_REACHED,
            )),
            AccountSubscribe => self.account_subscribe(request).await,
            ProgramSubscribe => self.program_subscribe(request).await,
            SignatureSubscribe => self.signature_subscribe(request).await,
            SlotSubscribe => self.slot_subscribe().await,
            LogsSubscribe => self.logs_subscribe(request).await,
            TransactionFailureSubscribe => self.transaction_failure_subscribe().await,
        }
    }

    fn has_subscription_slot(&mut self) -> bool {
        if self.tasks.len() < MAX_SUBSCRIPTIONS {
            return true;
        }
        // Finished one-shots only need to be reaped when we run out of space.
        self.tasks.retain(|_, task| !task.is_finished());
        self.tasks.len() < MAX_SUBSCRIPTIONS
    }

    fn unsubscribe(&mut self, request: &JsonWsRequest) -> RpcResult<SubResult> {
        let id = request.required::<SubscriptionID>(0)?;

        let success = self
            .tasks
            .remove(&id)
            .filter(|handle| !handle.is_finished())
            .inspect(JoinHandle::abort)
            .is_some();
        Ok(SubResult::Unsub(success))
    }

    /// Forward every streaming subscription through the same delivery loop.
    pub(crate) fn forward<T: Send + 'static>(
        &mut self,
        mut rx: mpsc::Receiver<T>,
        encode: impl Fn(SubscriptionID, T) -> RpcResult<Option<Bytes>> + Send + 'static,
    ) -> SubResult {
        let cancel = self.cancel.clone();
        self.spawn_subscription(move |id, tx| async move {
            while let Some(update) = rx.recv().await {
                let bytes = match encode(id, update) {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => continue,
                    Err(error) => {
                        warn!(
                            ?error,
                            "subscription serialization failed; closing WebSocket"
                        );
                        break;
                    }
                };
                if tx.send(bytes).await.is_err() {
                    return;
                }
            }
            cancel.cancel();
        })
    }

    /// Starts an admitted subscription and records the task under the ID sent to the client.
    pub(crate) fn spawn_subscription<F>(
        &mut self,
        run: impl FnOnce(SubscriptionID, mpsc::Sender<Bytes>) -> F,
    ) -> SubResult
    where
        F: Future<Output = ()> + Send + 'static,
    {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);

        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let handle = tokio::spawn(run(id, self.tx.clone()));
        if let Some(previous) = self.tasks.insert(id, handle) {
            previous.abort();
        }
        SubResult::SubId(id)
    }
}

impl Drop for WsDispatcher {
    /// Aborts every forwarding task when the connection goes away.
    fn drop(&mut self) {
        for handle in self.tasks.values() {
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
