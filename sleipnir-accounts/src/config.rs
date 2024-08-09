use sleipnir_mutator::Cluster;

#[derive(Debug, PartialEq, Eq)]
pub struct AccountsConfig {
    pub cluster: Cluster,
    pub lifecycle: LifecycleMode,
    pub commit_compute_unit_price: u64,
    pub payer_init_lamports: Option<u64>,
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
    pub fn disable_cloning(&self) -> bool {
        match self {
            LifecycleMode::ChainWithPrograms => false,
            LifecycleMode::ChainWithAnything => false,
            LifecycleMode::EphemeralWithPrograms => false,
            LifecycleMode::EphemeralWithAnything => false,
            LifecycleMode::Isolated => true,
        }
    }
    pub fn allow_cloning_non_programs(&self) -> bool {
        match self {
            LifecycleMode::ChainWithPrograms => false,
            LifecycleMode::ChainWithAnything => true,
            LifecycleMode::EphemeralWithPrograms => false,
            LifecycleMode::EphemeralWithAnything => true,
            LifecycleMode::Isolated => false,
        }
    }
    pub fn require_ephemeral_validation(&self) -> bool {
        match self {
            LifecycleMode::ChainWithPrograms => false,
            LifecycleMode::ChainWithAnything => false,
            LifecycleMode::EphemeralWithPrograms => true,
            LifecycleMode::EphemeralWithAnything => true,
            LifecycleMode::Isolated => false,
        }
    }
}
