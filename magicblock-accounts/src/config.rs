use std::collections::HashSet;

use magicblock_account_cloner::AccountClonerPermissions;
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
    // TODO(thlorenz): @@ adapt this to current pipeline and include this once
    // we support all lifecycle modes again.
    // Mainly we still should need:
    // - allow_cloning_refresh
    // - allow_cloning_undelegated_accounts
    // - allow_cloning_delegated_accounts
    // - allow_cloning_program_accounts
    pub fn to_account_cloner_permissions(&self) -> AccountClonerPermissions {
        match self {
            LifecycleMode::Replica => AccountClonerPermissions {
                allow_cloning_refresh: false,
                allow_cloning_feepayer_accounts: true,
                allow_cloning_undelegated_accounts: true,
                allow_cloning_delegated_accounts: true,
                allow_cloning_program_accounts: true,
            },
            LifecycleMode::ProgramsReplica => AccountClonerPermissions {
                allow_cloning_refresh: false,
                allow_cloning_feepayer_accounts: false,
                allow_cloning_undelegated_accounts: false,
                allow_cloning_delegated_accounts: false,
                allow_cloning_program_accounts: true,
            },
            LifecycleMode::Ephemeral => AccountClonerPermissions {
                allow_cloning_refresh: true,
                allow_cloning_feepayer_accounts: true,
                allow_cloning_undelegated_accounts: true,
                allow_cloning_delegated_accounts: true,
                allow_cloning_program_accounts: true,
            },
            LifecycleMode::Offline => AccountClonerPermissions {
                allow_cloning_refresh: false,
                allow_cloning_feepayer_accounts: false,
                allow_cloning_undelegated_accounts: false,
                allow_cloning_delegated_accounts: false,
                allow_cloning_program_accounts: false,
            },
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
