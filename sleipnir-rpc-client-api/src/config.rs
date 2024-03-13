// NOTE: pieces extracted from rpc-client-api/src/config.rs
use serde::{Deserialize, Serialize};
use solana_sdk::{clock::Slot, commitment_config::CommitmentConfig};

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

pub use solana_account_decoder::{
    UiAccount, UiAccountEncoding, UiDataSliceConfig,
};
