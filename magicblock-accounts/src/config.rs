use std::collections::HashSet;

use magicblock_mutator::Cluster;
use solana_sdk::pubkey::Pubkey;

#[derive(Debug, PartialEq, Eq)]
pub struct AccountsConfig {
    pub remote_cluster: Cluster,
    pub lifecycle: LifecycleMode,
    pub commit_compute_unit_price: u64,
    pub allowed_program_ids: Option<HashSet<Pubkey>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LifecycleMode {
    Replica,
    ProgramsReplica,
    Ephemeral,
    Offline,
}

impl LifecycleMode {
    pub fn requires_ephemeral_validation(&self) -> bool {
        match self {
            LifecycleMode::Replica => false,
            LifecycleMode::ProgramsReplica => false,
            LifecycleMode::Ephemeral => true,
            LifecycleMode::Offline => false,
        }
    }
}
