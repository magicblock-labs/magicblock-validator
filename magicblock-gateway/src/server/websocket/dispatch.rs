use std::{collections::HashMap, sync::Arc};

use crate::{
    error::RpcError,
    requests::JsonRpcMethod,
    state::{
        signatures::SignaturesExpirer,
        subscriptions::{CleanUp, SubscriptionID, SubscriptionsDb},
        transactions::TransactionsCache,
        SharedState,
    },
    RpcResult,
};

use super::{connection::ConnectionID, JsonRequest};
use hyper::body::Bytes;
use json::{Serialize, Value};
use tokio::sync::mpsc;

pub(crate) type ConnectionTx = mpsc::Sender<Bytes>;

pub(crate) struct WsDispatcher {
    subscriptions: SubscriptionsDb,
    unsubs: HashMap<SubscriptionID, CleanUp>,
    signatures: SignaturesExpirer,
    transactions: Arc<TransactionsCache>,
    chan: WsConnectionChannel,
}

impl WsDispatcher {
    pub(crate) fn new(state: SharedState, chan: WsConnectionChannel) -> Self {
        Self {
            subscriptions: state.subscriptions,
            unsubs: Default::default(),
            signatures: SignaturesExpirer::init(),
            transactions: state.transactions,
            chan,
        }
    }
    pub(crate) async fn dispatch(
        &self,
        request: JsonRequest,
    ) -> RpcResult<WsDispatchResult> {
        use JsonRpcMethod::*;
        match request.method {
            AccountSubscribe => {}
            AccountUnsubscribe => {}
            ProgramSubscribe => {}
            ProgramUnsubscribe => {}
            SlotSubscribe => {}
            SlotUnsubsribe => {}
            LogsSubscribe => {}
            LogsUnsubscribe => {}
            unknown => return Err(RpcError::method_not_found(unknown)),
        }
        todo!()
    }

    pub(crate) async fn cleanup(&mut self) {
        let signature = self.signatures.step().await;
        self.subscriptions.signatures.remove_async(&signature);
    }
}

pub(crate) struct WsConnectionChannel {
    pub(crate) id: ConnectionID,
    pub(crate) tx: ConnectionTx,
}

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum SubResult {
    SubId(u64),
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
