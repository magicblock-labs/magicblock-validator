use jsonrpc_core::Params;
use serde::{Deserialize, Serialize};
use sleipnir_rpc_client_api::{
    config::{
        RpcAccountInfoConfig, RpcSignatureSubscribeConfig, UiAccountEncoding,
        UiDataSliceConfig,
    },
    response::{Response, RpcResponseContext},
};
use solana_sdk::commitment_config::CommitmentLevel;

// -----------------
// AccountParams
// -----------------
#[derive(Serialize, Deserialize, Debug)]
pub struct AccountParams(String, Option<RpcAccountInfoConfig>);

#[allow(unused)]
impl AccountParams {
    pub fn pubkey(&self) -> &str {
        &self.0
    }

    pub fn encoding(&self) -> Option<UiAccountEncoding> {
        self.config().as_ref().and_then(|x| x.encoding)
    }

    pub fn commitment(&self) -> Option<CommitmentLevel> {
        self.config()
            .as_ref()
            .and_then(|x| x.commitment.map(|c| c.commitment))
    }

    pub fn data_slice_config(&self) -> Option<UiDataSliceConfig> {
        self.config().as_ref().and_then(|x| x.data_slice)
    }

    fn config(&self) -> &Option<RpcAccountInfoConfig> {
        &self.1
    }
}

// -----------------
// SignatureParams
// -----------------
#[derive(Serialize, Deserialize, Debug)]
pub struct SignatureParams(String, Option<RpcSignatureSubscribeConfig>);
impl SignatureParams {
    pub fn signature(&self) -> &str {
        &self.0
    }

    #[allow(unused)]
    pub fn config(&self) -> &Option<RpcSignatureSubscribeConfig> {
        &self.1
    }
}

// -----------------
// SlotResponse
// -----------------
#[derive(Serialize, Debug)]
pub struct SlotResponse {
    pub parent: u64,
    pub root: u64,
    pub slot: u64,
}

// -----------------
// ReponseNoContextWithSubscriptionId
// -----------------
#[derive(Serialize, Debug)]
pub struct ReponseNoContextWithSubscriptionId<T: Serialize> {
    pub response: T,
    pub subscription: u64,
}

impl<T: Serialize> ReponseNoContextWithSubscriptionId<T> {
    pub fn new(result: T, subscription: u64) -> Self {
        Self {
            response: result,
            subscription,
        }
    }

    fn into_value_map(self) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        map.insert(
            "result".to_string(),
            serde_json::to_value(self.response).unwrap(),
        );
        map.insert(
            "subscription".to_string(),
            serde_json::to_value(self.subscription).unwrap(),
        );
        map
    }

    pub fn into_params_map(self) -> Params {
        Params::Map(self.into_value_map())
    }
}

// -----------------
// ResponseWithSubscriptionId
// -----------------
#[derive(Serialize, Debug)]
pub struct ResponseWithSubscriptionId<T: Serialize> {
    pub response: Response<T>,
    pub subscription: u64,
}

impl<T: Serialize> ResponseWithSubscriptionId<T> {
    pub fn new(result: T, slot: u64, subscription: u64) -> Self {
        let response = Response {
            context: RpcResponseContext::new(slot),
            value: result,
        };
        Self {
            response,
            subscription,
        }
    }
}

impl<T: Serialize> ResponseWithSubscriptionId<T> {
    fn into_value_map(self) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        map.insert(
            "result".to_string(),
            serde_json::to_value(self.response).unwrap(),
        );
        map.insert(
            "subscription".to_string(),
            serde_json::to_value(self.subscription).unwrap(),
        );
        map
    }

    pub fn into_params_map(self) -> Params {
        Params::Map(self.into_value_map())
    }
}
