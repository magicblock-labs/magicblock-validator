use std::collections::HashSet;

use sleipnir_mutator::Cluster;
use solana_sdk::pubkey::Pubkey;

#[derive(Debug, PartialEq, Eq)]
pub struct AccountsConfig {
    pub remote_cluster: Cluster,
    pub lifecycle: LifecycleMode,
    pub commit_compute_unit_price: u64,
    pub payer_init_lamports: Option<u64>,
    pub whitelisted_program_ids: Option<HashSet<Pubkey>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LifecycleMode {
    Replica,
    ProgramsReplica,
    Ephemeral,
    Offline,
}

impl LifecycleMode {
    pub fn allow_cloning_new_accounts(&self) -> bool {
        match self {
            LifecycleMode::Replica => true,
            LifecycleMode::ProgramsReplica => false,
            LifecycleMode::Ephemeral => true,
            LifecycleMode::Offline => false,
        }
    }
    pub fn allow_cloning_payer_accounts(&self) -> bool {
        match self {
            LifecycleMode::Replica => true,
            LifecycleMode::ProgramsReplica => false,
            LifecycleMode::Ephemeral => true,
            LifecycleMode::Offline => false,
        }
    }
    pub fn allow_cloning_pda_accounts(&self) -> bool {
        match self {
            LifecycleMode::Replica => true,
            LifecycleMode::ProgramsReplica => false,
            LifecycleMode::Ephemeral => true,
            LifecycleMode::Offline => false,
        }
    }
    pub fn allow_cloning_delegated_accounts(&self) -> bool {
        match self {
            LifecycleMode::Replica => true,
            LifecycleMode::ProgramsReplica => false,
            LifecycleMode::Ephemeral => true,
            LifecycleMode::Offline => false,
        }
    }
    pub fn allow_cloning_program_accounts(&self) -> bool {
        match self {
            LifecycleMode::Replica => true,
            LifecycleMode::ProgramsReplica => true,
            LifecycleMode::Ephemeral => true,
            LifecycleMode::Offline => false,
        }
    }
    pub fn requires_ephemeral_validation(&self) -> bool {
        match self {
            LifecycleMode::Replica => false,
            LifecycleMode::ProgramsReplica => false,
            LifecycleMode::Ephemeral => true,
            LifecycleMode::Offline => false,
        }
    }
}
