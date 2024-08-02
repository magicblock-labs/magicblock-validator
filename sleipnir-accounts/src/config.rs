use sleipnir_mutator::Cluster;
use solana_sdk::genesis_config::ClusterType;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AccountsConfig {
    pub external: ExternalConfig,
    pub create: bool,
    pub commit_compute_unit_price: u64,
    pub payer_init_lamports: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ExternalConfig {
    pub cluster: Cluster,
    pub mode: ExternalCloneMode,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExternalCloneMode {
    All,
    ProgramsOnly,
    None,
}

impl Default for ExternalConfig {
    fn default() -> Self {
        Self {
            cluster: Cluster::Known(ClusterType::Devnet),
            mode: ExternalCloneMode::All,
        }
    }
}
