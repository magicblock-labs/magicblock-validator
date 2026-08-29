use json::{Array, Deserialize, JsonValueTrait, Value};
use serde::de::DeserializeOwned;

use crate::{RpcResult, error::RpcError};

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
/// Represents either a single JSON-RPC request or a batch of multiple requests.
pub enum RpcRequest {
    Single(JsonHttpRequest),
    Multi(Vec<JsonHttpRequest>),
}

impl<M> JsonRequest<M> {
    pub(crate) fn required<T: DeserializeOwned>(
        &self,
        index: usize,
    ) -> RpcResult<T> {
        let value = self
            .params
            .as_ref()
            .and_then(|params| params.get(index))
            .ok_or_else(|| {
            RpcError::invalid_params(format!("missing parameter {index}"))
        })?;
        json::from_value(value).map_err(RpcError::invalid_params)
    }

    pub(crate) fn optional<T: DeserializeOwned>(
        &self,
        index: usize,
    ) -> RpcResult<Option<T>> {
        self.params
            .as_ref()
            .and_then(|params| params.get(index))
            .map(|value| {
                if value.is_null() {
                    Ok(None)
                } else {
                    json::from_value(value)
                        .map(Some)
                        .map_err(RpcError::invalid_params)
                }
            })
            .transpose()
            .map(Option::flatten)
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
    /// Custom Magic Router-compatible method: mocked on validator.
    GetRoutes,
    /// Custom Magic Router-compatible method: alias of `getLatestBlockhash` on validator.
    GetBlockhashForAccounts,
    /// Custom Magic Router-compatible method: exposes simple delegation flag.
    GetDelegationStatus,
    #[serde(other)]
    MethodNotFound,
}

/// All supported JSON-RPC Websocket method names.
#[derive(json::Deserialize, Debug, Copy, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) enum JsonRpcWsMethod {
    AccountSubscribe,
    AccountUnsubscribe,
    LogsSubscribe,
    LogsUnsubscribe,
    Ping,
    ProgramSubscribe,
    ProgramUnsubscribe,
    SignatureSubscribe,
    SignatureUnsubscribe,
    SlotSubscribe,
    SlotUnsubscribe,
    #[serde(other)]
    MethodNotFound,
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
            Self::GetRoutes => "getRoutes",
            Self::GetBlockhashForAccounts => "getBlockhashForAccounts",
            Self::GetDelegationStatus => "getDelegationStatus",
            Self::MethodNotFound => "methodNotFound",
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
            Self::Ping => "ping",
            Self::ProgramSubscribe => "programSubscribe",
            Self::ProgramUnsubscribe => "programUnsubscribe",
            Self::SignatureSubscribe => "signatureSubscribe",
            Self::SignatureUnsubscribe => "signatureUnsubscribe",
            Self::SlotSubscribe => "slotSubscribe",
            Self::SlotUnsubscribe => "slotUnsubscribe",
            Self::MethodNotFound => "methodNotFound",
        }
    }
}

pub(crate) mod http;
pub(crate) mod params;
pub(crate) mod payload;
pub(crate) mod websocket;
