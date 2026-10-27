use std::time::Duration;

use solana_commitment_config::CommitmentConfig;

use crate::shared::compute_budget::ComputeBudgetConfig;

pub const DEFAULT_ACTIONS_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct CommittorConfig {
    pub rpc_uri: String,
    pub websocket_uri: Option<String>,
    pub commitment: CommitmentConfig,
    pub compute_budget_config: ComputeBudgetConfig,
    pub actions_timeout: Duration,
    pub slot_interval: Duration,
}
