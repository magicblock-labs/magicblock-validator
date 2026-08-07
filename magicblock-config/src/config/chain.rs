use std::time::Duration;

use serde::{Deserialize, Serialize};
use solana_pubkey::Pubkey;

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

/// Optional leader-owned administrative background work.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AdminConfig {
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
    /// If specified, only these programs will be cloned into the validator.
    /// If empty or not specified, all programs are allowed.
    pub allowed_programs: Option<Vec<AllowedProgram>>,

    /// Delay between resubscribing to accounts after a pubsub
    /// reconnection. This throttles the rate at which we resubscribe to prevent
    /// overwhelming the RPC provider. Default is 50ms.
    #[serde(with = "humantime")]
    pub resubscription_delay: Duration,

    /// Period for polling DLP-owned UndelegationRequest accounts.
    /// Set to 0s to disable the polling backfill loop.
    #[serde(with = "humantime")]
    pub undelegation_request_poll_interval: Duration,

    /// Address risk checks for post-delegation actions via the risk server.
    pub risk: RiskConfig,
}

impl Default for ChainLinkConfig {
    fn default() -> Self {
        Self {
            allowed_programs: None,
            resubscription_delay: Duration::from_millis(
                consts::DEFAULT_RESUBSCRIPTION_DELAY_MS,
            ),
            undelegation_request_poll_interval: Duration::from_secs(
                consts::DEFAULT_UNDELEGATION_REQUEST_POLL_INTERVAL_SECS,
            ),
            risk: RiskConfig::default(),
        }
    }
}

/// Strategy for deciding which post-delegation action signers get AML/risk
/// checked.
#[derive(
    Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum AmlCheckStrategy {
    /// Check every signer of every post-delegation action, regardless of which
    /// programs the action invokes.
    AllSigners,
    /// Only check signers when a post-delegation action involves the SPL Token,
    /// ephemeral SPL (eATA/ESPL), or Magic program. Actions touching none of
    /// these programs skip the risk check entirely.
    #[default]
    RelevantPrograms,
}

/// Configuration for checking address risk against the risk server. The risk
/// server owns the upstream provider credentials, caching, and threshold; the
/// validator is a thin client.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct RiskConfig {
    /// Enables post-delegation address risk checks.
    pub enabled: bool,
    /// Base URL of the risk server that performs address risk assessments.
    pub risk_server_url: String,
    /// Request timeout for risk server calls.
    #[serde(with = "humantime")]
    pub request_timeout: Duration,
    /// Which post-delegation action signers to risk check.
    pub check_strategy: AmlCheckStrategy,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            risk_server_url: consts::DEFAULT_RISK_SERVER_URL.to_string(),
            request_timeout: Duration::from_secs(
                consts::DEFAULT_RISK_REQUEST_TIMEOUT_SEC,
            ),
            check_strategy: AmlCheckStrategy::default(),
        }
    }
}

/// A program that is allowed to be cloned into the validator.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AllowedProgram {
    /// The public key of the program.
    pub id: Pubkey,
}
