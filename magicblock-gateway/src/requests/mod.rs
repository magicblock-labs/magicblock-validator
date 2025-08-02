use std::fmt::Display;

use json::{Array, Value};

#[derive(json::Deserialize)]
pub(crate) struct JsonRequest {
    pub(crate) id: Value,
    pub(crate) method: JsonRpcMethod,
    pub(crate) params: Option<Array>,
}

#[derive(json::Deserialize, Debug, Copy, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) enum JsonRpcMethod {
    AccountSubscribe,
    AccountUnsubscribe,
    GetAccountInfo,
    GetBalance,
    GetBlock,
    GetBlockHeight,
    GetBlocks,
    GetIdentity,
    GetLatestBlockhash,
    GetMultipleAccounts,
    GetProgramAccounts,
    GetSignatureStatuses,
    GetSignaturesForAddress,
    GetSlot,
    GetTokenAccountsByDelegate,
    GetTokenAccountsByOwner,
    GetTransaction,
    LogsSubscribe,
    LogsUnsubscribe,
    ProgramSubscribe,
    ProgramUnsubscribe,
    SendTransaction,
    SignatureSubscribe,
    SignatureUnsubscribe,
    SimulateTransaction,
    SlotSubscribe,
    SlotUnsubsribe,
}

impl Display for JsonRpcMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

#[macro_export]
macro_rules! parse_params {
    ($input: expr, $ty1: ty) => {
        $input.pop().and_then(|v| json::from_value::<$ty1>(&v).ok())
    };
    ($input: expr, $ty1: ty, $ty2: ty) => {{
        $input.reverse();
        (parse_params!($input, $ty1), parse_params!($input, $ty2))
    }};
    ($input: expr, $ty1: ty, $ty2: ty, $ty3: ty) => {{
        $input.reverse();
        (
            parse_params!($input, $ty1),
            parse_params!($input, $ty2),
            parse_params!($input, $ty3),
        )
    }};
}

pub(crate) mod http;
pub(crate) mod params;
pub(crate) mod payload;
pub(crate) mod websocket;
