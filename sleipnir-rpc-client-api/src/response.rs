// NOTE: from rpc-client-api/src/response.rs without vote/token related parts
use std::{fmt, net::SocketAddr, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use solana_account_decoder::UiAccount;
use solana_sdk::{
    clock::{Epoch, Slot, UnixTimestamp},
    transaction::{Result, TransactionError},
};
use solana_transaction_status::{
    ConfirmedTransactionStatusWithSignature, TransactionConfirmationStatus,
    UiConfirmedBlock, UiInnerInstructions, UiTransactionReturnData,
};
use thiserror::Error;

use crate::client_error;

// -----------------
// OptionalContext
// -----------------

/// Wrapper for rpc return types of methods that provide responses both with and without context.
/// Main purpose of this is to fix methods that lack context information in their return type,
/// without breaking backwards compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OptionalContext<T> {
    Context(Response<T>),
    NoContext(T),
}

impl<T> OptionalContext<T> {
    pub fn parse_value(self) -> T {
        match self {
            Self::Context(response) => response.value,
            Self::NoContext(value) => value,
        }
    }
}

pub type RpcResult<T> = client_error::Result<Response<T>>;

// -----------------
// RpcResponseContext
// -----------------
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response<T> {
    pub context: RpcResponseContext,
    pub value: T,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcResponseContext {
    pub slot: Slot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_version: Option<RpcApiVersion>,
}
impl RpcResponseContext {
    pub fn new(slot: Slot) -> Self {
        Self {
            slot,
            api_version: Some(RpcApiVersion::default()),
        }
    }
}

// -----------------
// RpcApiVersion
// -----------------
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcApiVersion(semver::Version);

impl std::ops::Deref for RpcApiVersion {
    type Target = semver::Version;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Default for RpcApiVersion {
    fn default() -> Self {
        Self(sleipnir_version::Version::default().as_semver_version())
    }
}

impl Serialize for RpcApiVersion {
    fn serialize<S>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for RpcApiVersion {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: String = Deserialize::deserialize(deserializer)?;
        Ok(RpcApiVersion(
            semver::Version::from_str(&s).map_err(serde::de::Error::custom)?,
        ))
    }
}

// NOTE: left out
// - RpcBlockCommitment
// - RpcBlockhashFeeCalculator
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RpcBlockhash {
    pub blockhash: String,
    pub last_valid_block_height: u64,
}
// NOTE: left out
// - RpcFees
// - DeprecatedRpcFees
// - Fees
// - RpcFeeCalculator
// - RpcFeeRateGovernor
// - RpcInflationGovernor
// - RpcInflationRate

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RpcKeyedAccount {
    pub pubkey: String,
    pub account: UiAccount,
}

// NOTE: left out
// - SlotInfo
// - SlotTransactionStats
// - SlotUpdate

// -----------------
// RpcSignatureResult
// -----------------
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProcessedSignatureResult {
    pub err: Option<TransactionError>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ReceivedSignatureResult {
    ReceivedSignature,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase", untagged)]
pub enum RpcSignatureResult {
    ProcessedSignature(ProcessedSignatureResult),
    ReceivedSignature(ReceivedSignatureResult),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RpcContactInfo {
    /// Pubkey of the node as a base-58 string
    pub pubkey: String,
    /// Gossip port
    pub gossip: Option<SocketAddr>,
    /// Tpu UDP port
    pub tpu: Option<SocketAddr>,
    /// Tpu QUIC port
    pub tpu_quic: Option<SocketAddr>,
    /// JSON RPC port
    pub rpc: Option<SocketAddr>,
    /// WebSocket PubSub port
    pub pubsub: Option<SocketAddr>,
    /// Software version
    pub version: Option<String>,
    /// First 4 bytes of the FeatureSet identifier
    pub feature_set: Option<u32>,
    /// Shred version
    pub shred_version: Option<u16>,
}

// NOTE: left out
// - RpcBlockProductionRange
// - RpcBlockProduction

// -----------------
// RpcVersionInfo
// -----------------
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct RpcVersionInfo {
    /// The current version of solana-core
    pub solana_core: String,
    /// first 4 bytes of the FeatureSet identifier
    pub feature_set: Option<u32>,
}

impl fmt::Debug for RpcVersionInfo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.solana_core)
    }
}

impl fmt::Display for RpcVersionInfo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if let Some(version) = self.solana_core.split_whitespace().next() {
            // Display just the semver if possible
            write!(f, "{version}")
        } else {
            write!(f, "{}", self.solana_core)
        }
    }
}

// -----------------
// RpcIdentity
// -----------------
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct RpcIdentity {
    /// The current node identity pubkey
    pub identity: String,
}

// NOTE: left out RpcVote

// -----------------
// RpcVoteAccountStatus
// -----------------
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RpcVoteAccountStatus {
    pub current: Vec<RpcVoteAccountInfo>,
    pub delinquent: Vec<RpcVoteAccountInfo>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RpcVoteAccountInfo {
    /// Vote account address, as base-58 encoded string
    pub vote_pubkey: String,

    /// The validator identity, as base-58 encoded string
    pub node_pubkey: String,

    /// The current stake, in lamports, delegated to this vote account
    pub activated_stake: u64,

    /// An 8-bit integer used as a fraction (commission/MAX_U8) for rewards payout
    pub commission: u8,

    /// Whether this account is staked for the current epoch
    pub epoch_vote_account: bool,

    /// Latest history of earned credits for up to `MAX_RPC_VOTE_ACCOUNT_INFO_EPOCH_CREDITS_HISTORY` epochs
    ///   each tuple is (Epoch, credits, prev_credits)
    pub epoch_credits: Vec<(Epoch, u64, u64)>,

    /// Most recent slot voted on by this vote account (0 if no votes exist)
    pub last_vote: u64,

    /// Current root slot for this vote account (0 if no root slot exists)
    pub root_slot: Slot,
}

// -----------------
// RpcSignatureConfirmation
// -----------------
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RpcSignatureConfirmation {
    pub confirmations: usize,
    pub status: Result<()>,
}

// -----------------
// RpcSimulateTransactionResult
// -----------------
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RpcSimulateTransactionResult {
    pub err: Option<TransactionError>,
    pub logs: Option<Vec<String>>,
    pub accounts: Option<Vec<Option<UiAccount>>>,
    pub units_consumed: Option<u64>,
    pub return_data: Option<UiTransactionReturnData>,
    pub inner_instructions: Option<Vec<UiInnerInstructions>>,
}

// NOTE: left out RpcStorageTurn

// -----------------
// RpcAccountBalance
// -----------------
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RpcAccountBalance {
    pub address: String,
    pub lamports: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RpcSupply {
    pub total: u64,
    pub circulating: u64,
    pub non_circulating: u64,
    pub non_circulating_accounts: Vec<String>,
}

// NOTE: left out
// - StakeActivationState
// - RpcStakeActivation
// - RpcTokenAccountBalance

// -----------------
// RpcConfirmedTransactionStatusWithSignature
// -----------------
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcConfirmedTransactionStatusWithSignature {
    pub signature: String,
    pub slot: Slot,
    pub err: Option<TransactionError>,
    pub memo: Option<String>,
    pub block_time: Option<UnixTimestamp>,
    pub confirmation_status: Option<TransactionConfirmationStatus>,
}

impl From<ConfirmedTransactionStatusWithSignature>
    for RpcConfirmedTransactionStatusWithSignature
{
    fn from(value: ConfirmedTransactionStatusWithSignature) -> Self {
        let ConfirmedTransactionStatusWithSignature {
            signature,
            slot,
            err,
            memo,
            block_time,
        } = value;
        Self {
            signature: signature.to_string(),
            slot,
            err,
            memo,
            block_time,
            confirmation_status: None,
        }
    }
}

// -----------------
// RpcPerfSample
// -----------------
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcPerfSample {
    pub slot: Slot,
    pub num_transactions: u64,
    pub num_non_vote_transactions: Option<u64>,
    pub num_slots: u64,
    pub sample_period_secs: u16,
}

// -----------------
// RpcInflationReward
// -----------------
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcInflationReward {
    pub epoch: Epoch,
    pub effective_slot: Slot,
    pub amount: u64,            // lamports
    pub post_balance: u64,      // lamports
    pub commission: Option<u8>, // Vote account commission when the reward was credited
}

// -----------------
// RpcBlockUpdate
// -----------------
#[derive(Clone, Deserialize, Serialize, Debug, Error, Eq, PartialEq)]
pub enum RpcBlockUpdateError {
    #[error("block store error")]
    BlockStoreError,

    #[error("unsupported transaction version ({0})")]
    UnsupportedTransactionVersion(u8),
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RpcBlockUpdate {
    pub slot: Slot,
    pub block: Option<UiConfirmedBlock>,
    pub err: Option<RpcBlockUpdateError>,
}

// -----------------
// RpcSnapshotSlotInfo
// -----------------
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RpcSnapshotSlotInfo {
    pub full: Slot,
    pub incremental: Option<Slot>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RpcPrioritizationFee {
    pub slot: Slot,
    pub prioritization_fee: u64,
}

// NOTE: left out tests (rpc_perf_sample_serializes_num_non_vote_transactions)
