use std::fmt::Display;

use json::{Array, Value};

pub(crate) mod http;
pub(crate) mod websocket;

#[derive(json::Deserialize)]
pub(crate) struct JsonRequest {
    pub(crate) id: Value,
    pub(crate) method: JsonRpcMethod,
    pub(crate) params: Option<Array>,
}

#[derive(json::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) enum JsonRpcMethod {
    GetAccountInfo,
    GetBlock,
    GetBlocks,
    GetMultipleAccounts,
    GetProgramAccounts,
    GetSignatureStatuses,
    GetSignaturesForAddress,
    GetSlot,
    GetTokenAccountsByDelegate,
    GetTokenAccountsByOwner,
    GetTransaction,
    SendTransaction,
    SimulateTransaction,
    SignatureSubscribe,
    SignatureUnsubscribe,
    AccountSubscribe,
    AccountUnsubscribe,
    ProgramSubscribe,
    ProgramUnsubscribe,
    LogsSubscribe,
    LogsUnsubscribe,
    SlotSubscribe,
    SlotUnsubsribe,
}

impl Display for JsonRpcMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
