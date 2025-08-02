use std::collections::HashMap;

use crate::{
    error::RpcError,
    parse_params,
    requests::JsonRpcMethod,
    state::{
        signatures::SignaturesExpirer,
        subscriptions::{CleanUp, SubscriptionID, SubscriptionsDb},
        transactions::TransactionsCache,
    },
    RpcResult,
};

use super::{connection::ConnectionID, JsonRequest};
use hyper::body::Bytes;
use json::{Serialize, Value};
use tokio::sync::mpsc;

pub(crate) type ConnectionTx = mpsc::Sender<Bytes>;

pub(crate) struct WsDispatcher {
    pub(crate) subscriptions: SubscriptionsDb,
    pub(crate) unsubs: HashMap<SubscriptionID, CleanUp>,
    pub(crate) signatures: SignaturesExpirer,
    pub(crate) transactions: TransactionsCache,
    pub(crate) chan: WsConnectionChannel,
}

impl WsDispatcher {
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
    pub(crate) async fn dispatch(
        &mut self,
        request: &mut JsonRequest,
    ) -> RpcResult<WsDispatchResult> {
        use JsonRpcMethod::*;
        let result = match request.method {
            AccountSubscribe => self.account_subscribe(request).await,
            ProgramSubscribe => self.program_subscribe(request).await,
            SignatureSubscribe => self.signature_subscribe(request).await,
            SlotSubscribe => self.slot_subscribe(),
            LogsSubscribe => self.logs_subscribe(request),
            AccountUnsubscribe | ProgramUnsubscribe | LogsUnsubscribe
            | SlotUnsubsribe => self.unsubscribe(request),
            unknown => return Err(RpcError::method_not_found(unknown)),
        }?;
        Ok(WsDispatchResult {
            id: request.id.take(),
            result,
        })
    }

    #[inline]
    pub(crate) async fn cleanup(&mut self) {
        let signature = self.signatures.expire().await;
        self.subscriptions.signatures.remove_async(&signature).await;
    }

    fn unsubscribe(
        &mut self,
        request: &mut JsonRequest,
    ) -> RpcResult<SubResult> {
        let mut params = request
            .params
            .take()
            .ok_or_else(|| RpcError::invalid_request("missing params"))?;

        let id = parse_params!(params, SubscriptionID).ok_or_else(|| {
            RpcError::invalid_params("missing or invalid subscription id")
        })?;
        let success = self.unsubs.remove(&id).is_some();
        Ok(SubResult::Unsub(success))
    }
}

#[derive(Clone)]
pub(crate) struct WsConnectionChannel {
    pub(crate) id: ConnectionID,
    pub(crate) tx: ConnectionTx,
}

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum SubResult {
    SubId(SubscriptionID),
    Unsub(bool),
}

pub(crate) struct WsDispatchResult {
    pub(crate) id: Value,
    pub(crate) result: SubResult,
}

impl Drop for WsDispatcher {
    fn drop(&mut self) {
        for s in self.signatures.cache.drain(..) {
            self.subscriptions.signatures.remove(&s.signature);
        }
    }
}
