use jsonrpc_core::Params;
use serde::{Deserialize, Serialize};
use sleipnir_rpc_client_api::{
    config::{RpcAccountInfoConfig, RpcSignatureSubscribeConfig},
    response::Response,
};

// -----------------
// AccountParams
// -----------------
#[derive(Serialize, Deserialize, Debug)]
pub struct AccountParams((String, RpcAccountInfoConfig));

// -----------------
// SignatureParams
// -----------------
#[derive(Serialize, Deserialize, Debug)]
pub struct SignatureParams(String, RpcSignatureSubscribeConfig);
impl SignatureParams {
    pub fn signature(&self) -> &str {
        &self.0
    }

    #[allow(unused)]
    pub fn config(&self) -> &RpcSignatureSubscribeConfig {
        &self.1
    }
}

// -----------------
// ResponseWithSubscriptionId
// -----------------
#[derive(Serialize, Debug)]
pub struct ResponseWithSubscriptionId<T: Serialize> {
    pub result: Response<T>,
    pub subscription: u64,
}

impl<T: Serialize> ResponseWithSubscriptionId<T> {
    fn into_value_map(self) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        map.insert(
            "result".to_string(),
            serde_json::to_value(self.result).unwrap(),
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
