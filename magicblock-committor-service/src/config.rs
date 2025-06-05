use solana_sdk::commitment_config::CommitmentLevel;

use crate::compute_budget::ComputeBudgetConfig;

#[derive(Debug, Clone)]
pub struct ChainConfig {
    pub rpc_uri: String,
    pub commitment: CommitmentLevel,
    pub compute_budget_config: ComputeBudgetConfig,
}

impl ChainConfig {
    pub fn devnet(compute_budget_config: ComputeBudgetConfig) -> Self {
        Self {
            rpc_uri: "https://api.devnet.solana.com".to_string(),
            commitment: CommitmentLevel::Confirmed,
            compute_budget_config,
        }
    }

    pub fn mainnet(compute_budget_config: ComputeBudgetConfig) -> Self {
        Self {
            rpc_uri: "https://api.mainnet-beta.solana.com".to_string(),
            commitment: CommitmentLevel::Confirmed,
            compute_budget_config,
        }
    }

    pub fn local(compute_budget_config: ComputeBudgetConfig) -> Self {
        Self {
            rpc_uri: "http://localhost:7799".to_string(),
            commitment: CommitmentLevel::Processed,
            compute_budget_config,
        }
    }
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self::local(ComputeBudgetConfig::new(1_000_000))
    }
}
