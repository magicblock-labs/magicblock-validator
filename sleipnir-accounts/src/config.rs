use sleipnir_mutator::Cluster;
use solana_sdk::genesis_config::ClusterType;

#[derive(Debug, PartialEq, Eq)]
pub struct AccountsConfig {
    pub cluster: Cluster,
    pub lifecycle: LifecycleMode,
    pub commit_compute_unit_price: u64,
    pub payer_init_lamports: Option<u64>,
}

impl Default for AccountsConfig {
    fn default() -> Self {
        Self {
            cluster: Cluster::Known(ClusterType::Devnet),
            lifecycle: LifecycleMode::ChainWithEverything,
            commit_compute_unit_price: 0,
            payer_init_lamports: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum LifecycleMode {
    ChainWithEverything,     // chain-full
    ChainWithPrograms,       // chain-programs
    EphemeralWithEverything, // ephem-full
    EphemeralWithPrograms,   // ephem-programs
    Isolated,                // isolated
}

impl LifecycleMode {
    pub fn is_ephem(&self) -> bool {
        match self {
            LifecycleMode::ChainWithEverything => false,
            LifecycleMode::ChainWithPrograms => false,
            LifecycleMode::EphemeralWithEverything => true,
            LifecycleMode::EphemeralWithPrograms => true,
            LifecycleMode::Isolated => false,
        }
    }
    pub fn is_programs_only(&self) -> bool {
        match self {
            LifecycleMode::ChainWithEverything => false,
            LifecycleMode::ChainWithPrograms => true,
            LifecycleMode::EphemeralWithEverything => false,
            LifecycleMode::EphemeralWithPrograms => true,
            LifecycleMode::Isolated => false,
        }
    }
}
