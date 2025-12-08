use solana_commitment_config::CommitmentConfig;

use crate::compute_budget::ComputeBudgetConfig;

#[derive(Debug, Clone)]
pub struct ChainConfig {
    pub rpc_uri: String,
    pub commitment: CommitmentConfig,
    pub compute_budget_config: ComputeBudgetConfig,
}

impl ChainConfig {
    pub fn devnet(compute_budget_config: ComputeBudgetConfig) -> Self {
        Self {
            rpc_uri: "https://api.devnet.solana.com".to_string(),
            commitment: CommitmentConfig::confirmed(),
            compute_budget_config,
        }
    }

    pub fn mainnet(compute_budget_config: ComputeBudgetConfig) -> Self {
        Self {
            rpc_uri: "https://api.mainnet-beta.solana.com".to_string(),
            commitment: CommitmentConfig::confirmed(),
            compute_budget_config,
        }
    }

    pub fn local(compute_budget_config: ComputeBudgetConfig) -> Self {
        Self {
            rpc_uri: "http://localhost:7799".to_string(),
            commitment: CommitmentConfig::processed(),
            compute_budget_config,
        }
    }
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self::local(ComputeBudgetConfig::new(1_000_000))
    }
}
