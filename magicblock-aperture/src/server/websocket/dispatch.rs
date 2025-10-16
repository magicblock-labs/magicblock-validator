use std::collections::HashMap;

use hyper::body::Bytes;
use json::{Serialize, Value};
use tokio::sync::mpsc;

use super::connection::ConnectionID;
use crate::{
    error::RpcError,
    parse_params,
    requests::{JsonRpcWsMethod, JsonWsRequest},
    state::{
        signatures::SignaturesExpirer,
        subscriptions::{CleanUp, SubscriptionID, SubscriptionsDb},
        transactions::TransactionsCache,
    },
    RpcResult,
};

/// The sender half of an MPSC channel used to push subscription notifications
/// to a single WebSocket client.
pub(crate) type ConnectionTx = mpsc::Sender<Bytes>;

/// The stateful request dispatcher for a single WebSocket connection.
///
/// An instance of `WsDispatcher` is created for each connected client and is
/// responsible for managing that client's specific set of subscriptions and their
/// lifecycles. It holds all the state necessary to process subscribe and
/// unsubscribe requests from that one client.
pub(crate) struct WsDispatcher {
    /// A handle to the global subscription database.
    pub(crate) subscriptions: SubscriptionsDb,
    /// A map storing the RAII `CleanUp` guards for this connection's active subscriptions.
    /// The key is the public `SubscriptionID` returned to the client. When a `CleanUp`
    /// guard is removed from this map, it is dropped, and its unsubscription logic is
    /// automatically executed.
    pub(crate) unsubs: HashMap<SubscriptionID, CleanUp>,
    /// A per-connection expirer for one-shot `signatureSubscribe` requests.
    pub(crate) signatures: SignaturesExpirer,
    /// A handle to the global transactions cache.
    pub(crate) transactions: TransactionsCache,
    /// The communication channel for this specific connection.
    pub(crate) chan: WsConnectionChannel,
}

impl WsDispatcher {
    /// Creates a new dispatcher for a single client connection.
    pub(crate) fn new(
        subscriptions: SubscriptionsDb,
        transactions: TransactionsCache,
        chan: WsConnectionChannel,
    ) -> Self {
        Self {
            subscriptions,
            unsubs: Default::default(),
            signatures: SignaturesExpirer::init(),
            transactions,
            chan,
        }
    }

    /// Routes an incoming JSON-RPC request to the appropriate subscription handler.
    pub(crate) async fn dispatch(
        &mut self,
        request: &mut JsonWsRequest,
    ) -> RpcResult<WsDispatchResult> {
        use JsonRpcWsMethod::*;
        let result = match request.method {
            AccountSubscribe => self.account_subscribe(request).await,
            ProgramSubscribe => self.program_subscribe(request).await,
            SignatureSubscribe => self.signature_subscribe(request).await,
            SlotSubscribe => self.slot_subscribe(),
            LogsSubscribe => self.logs_subscribe(request),
            AccountUnsubscribe | ProgramUnsubscribe | LogsUnsubscribe
            | SlotUnsubscribe | SignatureUnsubscribe => {
                self.unsubscribe(request)
            }
        }?;

        Ok(WsDispatchResult {
            id: request.id.take(),
            result,
        })
    }

    /// Performs periodic cleanup tasks for the connection.
    ///
    /// This is designed to be polled continuously in the connection's main event loop.
    /// Its primary job is to manage the lifecycle of one-shot `signatureSubscribe`
    /// requests, removing them from the global database if they expire before being fulfilled.
    #[inline]
    pub(crate) async fn cleanup(&mut self) {
        let signature = self.signatures.expire().await;
        // The subscription might have already been fulfilled and removed, so we
        // don't need to handle the case where `remove_async` finds nothing.
        self.subscriptions.signatures.remove_async(&signature).await;
    }

    /// Handles a request to unsubscribe from a previously established subscription.
    ///
    /// This works by removing the subscription's `CleanUp` guard from the `unsubs`
    /// map. When the guard is dropped, its associated cleanup logic is automatically
    /// executed in a background task, removing the subscriber from the global database.
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

        // `remove` returns `Some(value)` if the key was present.
        // Dropping the value triggers the unsubscription logic.
        let success = self.unsubs.remove(&id).is_some();
        Ok(SubResult::Unsub(success))
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
}

/// A container for a successfully processed RPC request, pairing the result with
/// the original request ID for the client to correlate.
pub(crate) struct WsDispatchResult {
    pub(crate) id: Value,
    pub(crate) result: SubResult,
}

impl Drop for WsDispatcher {
    /// Ensures all of a client's pending `signatureSubscribe` requests
    /// are removed from the global database when the client disconnects.
    fn drop(&mut self) {
        // Drain the per-connection cache and remove each corresponding entry from the
        // global signature subscription database to prevent orphans (memory leak)
        for s in self.signatures.cache.drain(..) {
            self.subscriptions.signatures.remove(&s.signature);
        }
    }
}
