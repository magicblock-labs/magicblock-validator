use json::{Array, Deserialize, Value};

use crate::{error::RpcError, RpcResult};

pub(crate) type JsonHttpRequest = JsonRequest<JsonRpcHttpMethod>;
pub(crate) type JsonWsRequest = JsonRequest<JsonRpcWsMethod>;

/// Represents a deserialized JSON-RPC 2.0 request object.
#[derive(Deserialize)]
pub(crate) struct JsonRequest<M> {
    /// The request identifier, which can be a string, number, or null.
    pub(crate) id: Value,
    /// The name of the RPC method to be invoked.
    pub(crate) method: M,
    /// An optional array of positional parameter values for the method.
    pub(crate) params: Option<Array>,
}

impl<M> JsonRequest<M> {
    /// A helper method to get a mutable reference to the
    /// `params` array, returning an error if it is `None`.
    fn params(&mut self) -> RpcResult<&mut Array> {
        self.params
            .as_mut()
            .ok_or_else(|| RpcError::invalid_request("missing params"))
    }
}

/// All supported JSON-RPC HTTP method names.
#[derive(json::Deserialize, Debug, Copy, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) enum JsonRpcHttpMethod {
    GetAccountInfo,
    GetBalance,
    GetBlock,
    GetBlockCommitment,
    GetBlockHeight,
    GetBlockTime,
    GetBlocks,
    GetBlocksWithLimit,
    GetClusterNodes,
    GetEpochInfo,
    GetEpochSchedule,
    GetFeeForMessage,
    GetFirstAvailableBlock,
    GetGenesisHash,
    GetHealth,
    GetHighestSnapshotSlot,
    GetIdentity,
    GetLargestAccounts,
    GetLatestBlockhash,
    GetMultipleAccounts,
    GetProgramAccounts,
    GetRecentPerformanceSamples,
    GetSignatureStatuses,
    GetSignaturesForAddress,
    GetSlot,
    GetSlotLeader,
    GetSlotLeaders,
    GetSupply,
    GetTokenAccountBalance,
    GetTokenAccountsByDelegate,
    GetTokenAccountsByOwner,
    GetTokenLargestAccounts,
    GetTokenSupply,
    GetTransaction,
    GetTransactionCount,
    GetVersion,
    GetVoteAccounts,
    IsBlockhashValid,
    MinimumLedgerSlot,
    RequestAirdrop,
    SendTransaction,
    SimulateTransaction,
}

/// All supported JSON-RPC Websocket method names.
#[derive(json::Deserialize, Debug, Copy, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) enum JsonRpcWsMethod {
    AccountSubscribe,
    AccountUnsubscribe,
    LogsSubscribe,
    LogsUnsubscribe,
    ProgramSubscribe,
    ProgramUnsubscribe,
    SignatureSubscribe,
    SignatureUnsubscribe,
    SlotSubscribe,
    SlotUnsubscribe,
}

impl JsonRpcHttpMethod {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::GetAccountInfo => "getAccountInfo",
            Self::GetBalance => "getBalance",
            Self::GetBlock => "getBlock",
            Self::GetBlockCommitment => "getBlockCommitment",
            Self::GetBlockHeight => "getBlockHeight",
            Self::GetBlockTime => "getBlockTime",
            Self::GetBlocks => "getBlocks",
            Self::GetBlocksWithLimit => "getBlocksWithLimit",
            Self::GetClusterNodes => "getClusterNodes",
            Self::GetEpochInfo => "getEpochInfo",
            Self::GetEpochSchedule => "getEpochSchedule",
            Self::GetFeeForMessage => "getFeeForMessage",
            Self::GetFirstAvailableBlock => "getFirstAvailableBlock",
            Self::GetGenesisHash => "getGenesisHash",
            Self::GetHealth => "getHealth",
            Self::GetHighestSnapshotSlot => "getHighestSnapshotSlot",
            Self::GetIdentity => "getIdentity",
            Self::GetLargestAccounts => "getLargestAccounts",
            Self::GetLatestBlockhash => "getLatestBlockhash",
            Self::GetMultipleAccounts => "getMultipleAccounts",
            Self::GetProgramAccounts => "getProgramAccounts",
            Self::GetRecentPerformanceSamples => "getRecentPerformanceSamples",
            Self::GetSignatureStatuses => "getSignatureStatuses",
            Self::GetSignaturesForAddress => "getSignaturesForAddress",
            Self::GetSlot => "getSlot",
            Self::GetSlotLeader => "getSlotLeader",
            Self::GetSlotLeaders => "getSlotLeaders",
            Self::GetSupply => "getSupply",
            Self::GetTokenAccountBalance => "getTokenAccountBalance",
            Self::GetTokenAccountsByDelegate => "getTokenAccountsByDelegate",
            Self::GetTokenAccountsByOwner => "getTokenAccountsByOwner",
            Self::GetTokenLargestAccounts => "getTokenLargestAccounts",
            Self::GetTokenSupply => "getTokenSupply",
            Self::GetTransaction => "getTransaction",
            Self::GetTransactionCount => "getTransactionCount",
            Self::GetVersion => "getVersion",
            Self::GetVoteAccounts => "getVoteAccounts",
            Self::IsBlockhashValid => "isBlockhashValid",
            Self::MinimumLedgerSlot => "minimumLedgerSlot",
            Self::RequestAirdrop => "requestAirdrop",
            Self::SendTransaction => "sendTransaction",
            Self::SimulateTransaction => "simulateTransaction",
        }
    }
}

impl JsonRpcWsMethod {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::AccountSubscribe => "accountSubscribe",
            Self::AccountUnsubscribe => "accountUnsubscribe",
            Self::LogsSubscribe => "logsSubscribe",
            Self::LogsUnsubscribe => "logsUnsubscribe",
            Self::ProgramSubscribe => "programSubscribe",
            Self::ProgramUnsubscribe => "programUnsubscribe",
            Self::SignatureSubscribe => "signatureSubscribe",
            Self::SignatureUnsubscribe => "signatureUnsubscribe",
            Self::SlotSubscribe => "slotSubscribe",
            Self::SlotUnsubscribe => "slotUnsubscribe",
        }
    }
}

/// A helper macro for easily parsing positional parameters from a JSON-RPC request.
///
/// This macro simplifies the process of extracting and deserializing parameters
/// from the `params` array of a `JsonRequest`.
///
/// ## Return Value
///
/// It returns an `Option<T>` for a single parameter, or a tuple of `Option<T>`s for
/// multiple parameters. Each `Option` will be `Some(value)` on a successful parse,
/// and `None` if a parameter is missing or fails to deserialize into the specified type.
#[macro_export]
macro_rules! parse_params {
    ($input: expr, $ty1: ty) => {{
        $input.reverse();
        $input.pop().and_then(|v| json::from_value::<$ty1>(&v).ok())
    }};
    (@reversed, $input: expr, $ty1: ty) => {
        $input.pop().and_then(|v| json::from_value::<$ty1>(&v).ok())
    };
    ($input: expr, $ty1: ty, $ty2: ty) => {{
        $input.reverse();
        (parse_params!(@reversed, $input, $ty1), parse_params!(@reversed, $input, $ty2))
    }};
    ($input: expr, $ty1: ty, $ty2: ty, $ty3: ty) => {{
        $input.reverse();
        (
            parse_params!(@reversed, $input, $ty1),
            parse_params!(@reversed, $input, $ty2),
            parse_params!(@reversed, $input, $ty3),
        )
    }};
}

pub(crate) mod http;
pub(crate) mod params;
pub(crate) mod payload;
pub(crate) mod websocket;
