use sleipnir_mutator::Cluster;
use solana_sdk::genesis_config::ClusterType;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AccountsConfig {
    pub external: ExternalConfig,
    pub create: bool,
    pub commit_compute_unit_price: u64,
    pub payer_init_lamports: Option<u64>,
}

// -----------------
// ExternalConfig
// -----------------
#[derive(Debug, PartialEq, Eq)]
pub struct ExternalConfig {
    pub cluster: Cluster,
    pub clone: CloneMode,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub enum CloneMode {
    Everything,
    #[default]
    ProgramsOnly,
    Nothing,
}

impl Default for ExternalConfig {
    fn default() -> Self {
        Self {
            cluster: Cluster::Known(ClusterType::Devnet),
            clone: Default::default(),
        }
    }
}
