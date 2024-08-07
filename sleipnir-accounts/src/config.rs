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
            lifecycle: LifecycleMode::ChainWithAnything,
            commit_compute_unit_price: 0,
            payer_init_lamports: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum LifecycleMode {
    ChainWithPrograms,
    ChainWithAnything,
    EphemeralWithPrograms,
    EphemeralWithAnything,
    Isolated,
}

impl LifecycleMode {
    pub fn allow_cloning_non_programs(&self) -> bool {
        match self {
            LifecycleMode::ChainWithPrograms => false,
            LifecycleMode::ChainWithAnything => true,
            LifecycleMode::EphemeralWithPrograms => false,
            LifecycleMode::EphemeralWithAnything => true,
            LifecycleMode::Isolated => false,
        }
    }
    pub fn require_delegation_for_writable(&self) -> bool {
        match self {
            LifecycleMode::ChainWithPrograms => false,
            LifecycleMode::ChainWithAnything => false,
            LifecycleMode::EphemeralWithPrograms => true,
            LifecycleMode::EphemeralWithAnything => true,
            LifecycleMode::Isolated => false,
        }
    }
    pub fn allow_new_account_for_writable(&self) -> bool {
        match self {
            LifecycleMode::ChainWithPrograms => true,
            LifecycleMode::ChainWithAnything => true,
            LifecycleMode::EphemeralWithPrograms => false,
            LifecycleMode::EphemeralWithAnything => false,
            LifecycleMode::Isolated => true,
        }
    }
}
