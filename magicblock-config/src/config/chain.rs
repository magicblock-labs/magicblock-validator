use std::time::Duration;

use isocountry::CountryCode;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use solana_pubkey::Pubkey;
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
    #[serde(default = "default_claim_fees_frequency", with = "humantime")]
    pub claim_fees_frequency: Duration,
}

fn default_claim_fees_frequency() -> Duration {
    Duration::from_secs(24 * 60 * 60)
}

/// Configuration for ChainLink (Cloning/BaseChain synchronization)
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct ChainLinkConfig {
    /// If true, initializes address lookup tables for oracle accounts.
    pub prepare_lookup_tables: bool,

    /// Amount of lamports to automatically airdrop to feepayer accounts on clone.
    pub auto_airdrop_lamports: u64,

    /// The maximum number of non-delegated accounts to track simultaneously for
    /// updates.
    pub max_monitored_accounts: usize,

    /// When true, confined accounts are removed during accounts bank reset.
    pub remove_confined_accounts: bool,

    /// If specified, only these programs will be cloned into the validator.
    /// If empty or not specified, all programs are allowed.
    pub allowed_programs: Option<Vec<AllowedProgram>>,

    /// Delay between resubscribing to accounts after a pubsub
    /// reconnection. This throttles the rate at which we resubscribe to prevent
    /// overwhelming the RPC provider. Default is 50ms.
    #[serde(with = "humantime")]
    pub resubscription_delay: Duration,

    /// AML/Risk checks for post-delegation actions via Range API.
    pub risk: RiskConfig,
}

impl Default for ChainLinkConfig {
    fn default() -> Self {
        Self {
            prepare_lookup_tables: false,
            auto_airdrop_lamports: 0,
            max_monitored_accounts: consts::DEFAULT_MAX_MONITORED_ACCOUNTS,
            remove_confined_accounts: false,
            allowed_programs: None,
            resubscription_delay: Duration::from_millis(
                consts::DEFAULT_RESUBSCRIPTION_DELAY_MS,
            ),
            risk: RiskConfig::default(),
        }
    }
}

/// Configuration for checking account risk with the Range API and a local sqlite cache.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct RiskConfig {
    /// Enables post-delegation address risk checks.
    pub enabled: bool,
    /// Base URL for the Range API.
    pub base_url: String,
    /// API key used for Range API authorization.
    pub api_key: Option<String>,
    /// TTL for cache entries.
    #[serde(with = "humantime")]
    pub cache_ttl: Duration,
    /// Request timeout for Range API calls.
    #[serde(with = "humantime")]
    pub request_timeout: Duration,
    /// Threshold on a scale of 0-10 for the risk score.
    pub risk_score_threshold: u64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: consts::DEFAULT_RISK_BASE_URL.to_string(),
            api_key: None,
            cache_ttl: Duration::from_secs(consts::DEFAULT_RISK_CACHE_TTL_SEC),
            request_timeout: Duration::from_secs(
                consts::DEFAULT_RISK_REQUEST_TIMEOUT_SEC,
            ),
            risk_score_threshold: consts::DEFAULT_RISK_SCORE_THRESHOLD,
        }
    }
}

/// A program that is allowed to be cloned into the validator.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AllowedProgram {
    /// The public key of the program.
    pub id: Pubkey,
}
