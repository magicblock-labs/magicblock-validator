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
    Replica,
    ProgramsReplica,
    Ephemeral,
    EphemeralLimited,
    Offline,
}

impl LifecycleMode {
    pub fn is_clone_readable_none(&self) -> bool {
        match self {
            LifecycleMode::Replica => false,
            LifecycleMode::ProgramsReplica => false,
            LifecycleMode::Ephemeral => false,
            LifecycleMode::EphemeralLimited => false,
            LifecycleMode::Offline => true,
        }
    }
    pub fn is_clone_readable_programs_only(&self) -> bool {
        match self {
            LifecycleMode::Replica => false,
            LifecycleMode::ProgramsReplica => true,
            LifecycleMode::Ephemeral => false,
            LifecycleMode::EphemeralLimited => true,
            LifecycleMode::Offline => false,
        }
    }

    pub fn is_clone_writable_none(&self) -> bool {
        match self {
            LifecycleMode::Replica => false,
            LifecycleMode::ProgramsReplica => true,
            LifecycleMode::Ephemeral => false,
            LifecycleMode::EphemeralLimited => false,
            LifecycleMode::Offline => true,
        }
    }

    pub fn requires_delegation_as_writable(&self) -> bool {
        match self {
            LifecycleMode::Replica => false,
            LifecycleMode::ProgramsReplica => false,
            LifecycleMode::Ephemeral => true,
            LifecycleMode::EphemeralLimited => true,
            LifecycleMode::Offline => false,
        }
    }
    pub fn allows_new_account_as_writable(&self) -> bool {
        match self {
            LifecycleMode::Replica => true,
            LifecycleMode::ProgramsReplica => true,
            LifecycleMode::Ephemeral => false,
            LifecycleMode::EphemeralLimited => false,
            LifecycleMode::Offline => true,
        }
    }
}
