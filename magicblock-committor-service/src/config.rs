use std::time::Duration;

use magicblock_rpc_client::BaseLayerBlockhash;
use solana_commitment_config::CommitmentConfig;

use crate::compute_budget::ComputeBudgetConfig;

pub const DEFAULT_ACTIONS_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct ChainConfig {
    pub rpc_uri: String,
    pub websocket_uri: Option<String>,
    pub commitment: CommitmentConfig,
    pub compute_budget_config: ComputeBudgetConfig,
    pub actions_timeout: Duration,
    pub blockhash_registry: Option<BaseLayerBlockhash>,
}

impl ChainConfig {
    pub fn devnet(compute_budget_config: ComputeBudgetConfig) -> Self {
        Self {
            rpc_uri: "https://api.devnet.solana.com".to_string(),
            websocket_uri: None,
            commitment: CommitmentConfig::confirmed(),
            compute_budget_config,
            actions_timeout: DEFAULT_ACTIONS_TIMEOUT,
            blockhash_registry: None,
        }
    }

    pub fn mainnet(compute_budget_config: ComputeBudgetConfig) -> Self {
        Self {
            rpc_uri: "https://api.mainnet-beta.solana.com".to_string(),
            websocket_uri: None,
            commitment: CommitmentConfig::confirmed(),
            compute_budget_config,
            actions_timeout: DEFAULT_ACTIONS_TIMEOUT,
            blockhash_registry: None,
        }
    }

    pub fn local(compute_budget_config: ComputeBudgetConfig) -> Self {
        Self {
            rpc_uri: "http://localhost:7799".to_string(),
            websocket_uri: None,
            commitment: CommitmentConfig::processed(),
            compute_budget_config,
            actions_timeout: DEFAULT_ACTIONS_TIMEOUT,
            blockhash_registry: None,
        }
    }
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self::local(ComputeBudgetConfig::new(1_000_000))
    }
}
