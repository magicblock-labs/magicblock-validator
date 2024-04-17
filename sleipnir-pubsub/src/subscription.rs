use log::*;
use std::sync::Arc;

use jsonrpc_core::Params;
use jsonrpc_pubsub::{Sink, Subscriber, SubscriptionId};
use serde::Serialize;
use sleipnir_geyser_plugin::rpc::GeyserRpcService;

use crate::types::AccountParams;

pub enum SubscriptionRequest {
    AccountSubscribe {
        subscriber: Subscriber,
        geyser_service: Arc<GeyserRpcService>,
        params: AccountParams,
    },
}

impl SubscriptionRequest {
    pub fn into_subscriber(self) -> Subscriber {
        match self {
            SubscriptionRequest::AccountSubscribe { subscriber, .. } => {
                subscriber
            }
        }
    }
}

// -----------------
// ResultWithSubscriptionId
// -----------------
#[derive(Serialize, Debug)]
pub struct ResultWithSubscriptionId<T: Serialize> {
    pub result: T,
    pub subid: u64,
}

impl<T: Serialize> ResultWithSubscriptionId<T> {
    pub fn new(result: T, subscription: u64) -> Self {
        Self {
            result,
            subid: subscription,
        }
    }

    fn into_value_map(self) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        map.insert(
            "result".to_string(),
            serde_json::to_value(self.result).unwrap(),
        );
        map.insert(
            "subid".to_string(),
            serde_json::to_value(self.subid).unwrap(),
        );
        map
    }

    pub fn into_params_map(self) -> Params {
        Params::Map(self.into_value_map())
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
