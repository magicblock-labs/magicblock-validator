use std::{
    collections::{HashMap, VecDeque},
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

use crate::{
    error::RpcError,
    requests::JsonRpcMethod,
    state::{
        subscriptions::{CleanUp, SubscriptionID, SubscriptionsDb},
        transactions::TransactionsCache,
        SharedState,
    },
    RpcResult,
};

use super::{connection::ConnectionID, JsonRequest};
use hyper::body::Bytes;
use json::{Serialize, Value};
use solana_signature::Signature;
use tokio::{
    sync::mpsc,
    time::{self, Interval},
};

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

struct SignaturesExpirer {
    signatures: VecDeque<ExpiringSignature>,
    tick: u64,
    ticker: Interval,
}

struct ExpiringSignature {
    ttl: u64,
    signature: Signature,
    subscribed: Arc<AtomicBool>,
}

impl SignaturesExpirer {
    const WAIT: u64 = 5;
    const TTL: u64 = 90 / Self::WAIT;
    fn init() -> Self {
        Self {
            signatures: Default::default(),
            tick: 0,
            ticker: time::interval(Duration::from_secs(Self::WAIT)),
        }
    }

    fn push(&mut self, signature: Signature, subscribed: Arc<AtomicBool>) {
        let sig = ExpiringSignature {
            signature,
            ttl: self.tick + Self::TTL,
            subscribed,
        };
        self.signatures.push_back(sig);
    }

    async fn step(&mut self) -> Signature {
        loop {
            'expire: {
                let Some(s) = self.signatures.front() else {
                    break 'expire;
                };
                if s.ttl > self.tick {
                    break 'expire;
                }
                let Some(s) = self.signatures.pop_front() else {
                    break 'expire;
                };
                if s.subscribed.load(std::sync::atomic::Ordering::Relaxed) {
                    return s.signature;
                }
            }
            self.ticker.tick().await;
            self.tick += 1;
        }
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
        for s in self.signatures.signatures.drain(..) {
            self.subscriptions.signatures.remove(&s.signature);
        }
    }
}
