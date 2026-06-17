use std::time::Duration;

use solana_commitment_config::CommitmentConfig;

use crate::compute_budget::ComputeBudgetConfig;

pub const DEFAULT_ACTIONS_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct ChainConfig {
    pub rpc_uri: String,
    pub photon_uri: Option<String>,
    pub websocket_uri: Option<String>,
    pub commitment: CommitmentConfig,
    pub compute_budget_config: ComputeBudgetConfig,
    pub actions_timeout: Duration,
}

impl ChainConfig {
    pub fn local(compute_budget_config: ComputeBudgetConfig) -> Self {
        Self {
            websocket_uri: None,
            rpc_uri: "http://localhost:7799".to_string(),
            photon_uri: Some("http://localhost:8784".to_string()),
            commitment: CommitmentConfig::processed(),
            compute_budget_config,
            actions_timeout: DEFAULT_ACTIONS_TIMEOUT,
        }
    }
}
