use std::fmt::Display;

use json::{Array, Value};

use crate::{error::RpcError, RpcResult};

#[derive(json::Deserialize)]
pub(crate) struct JsonRequest {
    pub(crate) id: Value,
    pub(crate) method: JsonRpcMethod,
    pub(crate) params: Option<Array>,
}

impl JsonRequest {
    fn params(&mut self) -> RpcResult<&mut Array> {
        self.params
            .as_mut()
            .ok_or_else(|| RpcError::invalid_request("missing params"))
    }
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
    GetBlocksWithLimit,
    GetIdentity,
    GetLatestBlockhash,
    GetMultipleAccounts,
    GetProgramAccounts,
    GetSignatureStatuses,
    GetSignaturesForAddress,
    GetSlot,
    GetTokenAccountBalance,
    GetTokenAccountsByDelegate,
    GetTokenAccountsByOwner,
    GetTransaction,
    IsBlockhashValid,
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
