use sleipnir_mutator::Cluster;

#[derive(Debug, PartialEq, Eq)]
pub struct AccountsConfig {
    pub remote_cluster: Cluster,
    pub lifecycle: LifecycleMode,
    pub commit_compute_unit_price: u64,
    pub payer_init_lamports: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LifecycleMode {
    Replica,
    ProgramsReplica,
    Ephemeral,
    EphemeralLimited,
    Offline,
}

impl LifecycleMode {
    pub fn is_offline(&self) -> bool {
        match self {
            LifecycleMode::Replica => false,
            LifecycleMode::ProgramsReplica => false,
            LifecycleMode::Ephemeral => false,
            LifecycleMode::EphemeralLimited => false,
            LifecycleMode::Offline => true,
        }
    }
    pub fn allow_cloning_system_accounts(&self) -> bool {
        match self {
            LifecycleMode::Replica => true,
            LifecycleMode::ProgramsReplica => false,
            LifecycleMode::Ephemeral => true,
            LifecycleMode::EphemeralLimited => true,
            LifecycleMode::Offline => false,
        }
    }
    pub fn allow_cloning_undelegated_non_programs(&self) -> bool {
        match self {
            LifecycleMode::Replica => true,
            LifecycleMode::ProgramsReplica => false,
            LifecycleMode::Ephemeral => true,
            LifecycleMode::EphemeralLimited => false,
            LifecycleMode::Offline => false,
        }
    }
    pub fn requires_ephemeral_validation(&self) -> bool {
        match self {
            LifecycleMode::Replica => false,
            LifecycleMode::ProgramsReplica => false,
            LifecycleMode::Ephemeral => true,
            LifecycleMode::EphemeralLimited => true,
            LifecycleMode::Offline => false,
        }
    }
}
