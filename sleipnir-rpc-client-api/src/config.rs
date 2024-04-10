// NOTE: pieces extracted from rpc-client-api/src/config.rs
use serde::{Deserialize, Serialize};
pub use solana_account_decoder::{
    UiAccount, UiAccountEncoding, UiDataSliceConfig,
};
use solana_sdk::{
    clock::{Epoch, Slot},
    commitment_config::{CommitmentConfig, CommitmentLevel},
};
use solana_transaction_status::UiTransactionEncoding;

use crate::filter::RpcFilterType;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcAccountInfoConfig {
    pub encoding: Option<UiAccountEncoding>,
    pub data_slice: Option<UiDataSliceConfig>,
    #[serde(flatten)]
    /// Commitment is ignored in our validator
    pub commitment: Option<CommitmentConfig>,
    /// Min context slot is ignored in our validator
    pub min_context_slot: Option<Slot>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcProgramAccountsConfig {
    pub filters: Option<Vec<RpcFilterType>>,
    #[serde(flatten)]
    pub account_config: RpcAccountInfoConfig,
    pub with_context: Option<bool>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcGetVoteAccountsConfig {
    pub vote_pubkey: Option<String>, // validator vote address, as a base-58 encoded string
    #[serde(flatten)]
    pub commitment: Option<CommitmentConfig>,
    pub keep_unstaked_delinquents: Option<bool>,
    pub delinquent_slot_distance: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSupplyConfig {
    #[serde(flatten)]
    pub commitment: Option<CommitmentConfig>,
    #[serde(default)]
    pub exclude_non_circulating_accounts_list: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcEpochConfig {
    pub epoch: Option<Epoch>,
    #[serde(flatten)]
    pub commitment: Option<CommitmentConfig>,
    pub min_context_slot: Option<Slot>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcRequestAirdropConfig {
    pub recent_blockhash: Option<String>, // base-58 encoded blockhash
    #[serde(flatten)]
    pub commitment: Option<CommitmentConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSignatureStatusConfig {
    pub search_transaction_history: bool,
}

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct RpcSendTransactionConfig {
    #[serde(default)]
    pub skip_preflight: bool,
    pub preflight_commitment: Option<CommitmentLevel>,
    pub encoding: Option<UiTransactionEncoding>,
    pub max_retries: Option<usize>,
    pub min_context_slot: Option<Slot>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSignaturesForAddressConfig {
    pub before: Option<String>, // Signature as base-58 string
    pub until: Option<String>,  // Signature as base-58 string
    pub limit: Option<usize>,
    #[serde(flatten)]
    pub commitment: Option<CommitmentConfig>,
    pub min_context_slot: Option<Slot>,
}

// -----------------
// RpcEncodingConfig
// -----------------
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcEncodingConfigWrapper<T> {
    Deprecated(Option<UiTransactionEncoding>),
    Current(Option<T>),
}

impl<T: EncodingConfig + Default + Copy> RpcEncodingConfigWrapper<T> {
    pub fn convert_to_current(&self) -> T {
        match self {
            RpcEncodingConfigWrapper::Deprecated(encoding) => {
                T::new_with_encoding(encoding)
            }
            RpcEncodingConfigWrapper::Current(config) => {
                config.unwrap_or_default()
            }
        }
    }

    pub fn convert<U: EncodingConfig + From<T>>(
        &self,
    ) -> RpcEncodingConfigWrapper<U> {
        match self {
            RpcEncodingConfigWrapper::Deprecated(encoding) => {
                RpcEncodingConfigWrapper::Deprecated(*encoding)
            }
            RpcEncodingConfigWrapper::Current(config) => {
                RpcEncodingConfigWrapper::Current(
                    config.map(|config| config.into()),
                )
            }
        }
    }
}

pub trait EncodingConfig {
    fn new_with_encoding(encoding: &Option<UiTransactionEncoding>) -> Self;
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct RpcTransactionConfig {
    pub encoding: Option<UiTransactionEncoding>,
    #[serde(flatten)]
    pub commitment: Option<CommitmentConfig>,
    pub max_supported_transaction_version: Option<u8>,
}

impl EncodingConfig for RpcTransactionConfig {
    fn new_with_encoding(encoding: &Option<UiTransactionEncoding>) -> Self {
        Self {
            encoding: *encoding,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcBlocksConfigWrapper {
    EndSlotOnly(Option<Slot>),
    CommitmentOnly(Option<CommitmentConfig>),
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct RpcContextConfig {
    #[serde(flatten)]
    pub commitment: Option<CommitmentConfig>,
    pub min_context_slot: Option<Slot>,
}

// -----------------
// Subscriptions
// -----------------
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSignatureSubscribeConfig {
    #[serde(flatten)]
    pub commitment: Option<CommitmentConfig>,
    pub enable_received_notification: Option<bool>,
}
