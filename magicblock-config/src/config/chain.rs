use std::time::Duration;

use isocountry::CountryCode;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use url::Url;

use crate::consts;

/// Strategy for committing transactions back to the base chain
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct CommittorConfig {
    /// The compute unit price (in micro-lamports) to set for commit transactions.
    /// Higher values increase inclusion priority on base chain.
    pub compute_unit_price: u64,
}

impl Default for CommittorConfig {
    fn default() -> Self {
        Self {
            compute_unit_price: consts::DEFAULT_COMPUTE_UNIT_PRICE,
        }
    }
}

/// Metadata and operational settings for on-chain validator registration.
#[serde_as]
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ChainOperationConfig {
    /// The validator's two-letter ISO country code (e.g., "US", "DE").
    pub country_code: CountryCode,

    /// The validator's fully qualified domain name (FQDN).
    pub fqdn: Url,

    /// Frequency at which the validator claims accrued fees from the chain.
    #[serde(with = "humantime")]
    pub claim_fees_frequency: Duration,
}

/// Configuration for ChainLink (Cloning/BaseChain synchronization)
#[derive(Deserialize, Serialize, Debug, Default, Clone)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct ChainLinkConfig {
    /// If true, initializes address lookup tables for oracle accounts.
    pub prepare_lookup_tables: bool,

    /// Amount of lamports to automatically airdrop to feepayer accounts on clone.
    pub auto_airdrop_lamports: u64,

    /// The maximum number of non-delegated accounts to track simultaneously for updates.
    pub max_monitored_accounts: usize,

    /// When true, confined accounts are removed during accounts bank reset.
    pub remove_confined_accounts: bool,
}
