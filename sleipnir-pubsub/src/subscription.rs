use std::sync::Arc;

use jsonrpc_pubsub::{Sink, Subscriber, SubscriptionId};
use log::*;
use sleipnir_bank::bank::Bank;
use sleipnir_geyser_plugin::rpc::GeyserRpcService;

use crate::types::{AccountParams, ProgramParams, SignatureParams};

pub enum SubscriptionRequest {
    Account {
        subscriber: Subscriber,
        geyser_service: Arc<GeyserRpcService>,
        params: AccountParams,
    },
    Program {
        subscriber: Subscriber,
        geyser_service: Arc<GeyserRpcService>,
        params: ProgramParams,
    },
    Slot {
        subscriber: Subscriber,
        geyser_service: Arc<GeyserRpcService>,
    },
    Signature {
        subscriber: Subscriber,
        geyser_service: Arc<GeyserRpcService>,
        params: SignatureParams,
        bank: Arc<Bank>,
    },
}

impl SubscriptionRequest {
    pub fn into_subscriber(self) -> Subscriber {
        use SubscriptionRequest::*;
        match self {
            Account { subscriber, .. } => subscriber,
            Program { subscriber, .. } => subscriber,
            Slot { subscriber, .. } => subscriber,
            Signature { subscriber, .. } => subscriber,
        }
    }
}

pub fn assign_sub_id(subscriber: Subscriber, subid: u64) -> Option<Sink> {
    match subscriber.assign_id(SubscriptionId::Number(subid)) {
        Ok(sink) => Some(sink),
        Err(err) => {
            error!("Failed to assign subscription id: {:?}", err);
            None
        }
    }
}
