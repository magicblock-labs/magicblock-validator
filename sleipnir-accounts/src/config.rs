use std::collections::HashSet;

use sleipnir_account_cloner::AccountClonerPermissions;
use sleipnir_mutator::Cluster;
use solana_sdk::pubkey::Pubkey;

#[derive(Debug, PartialEq, Eq)]
pub struct AccountsConfig {
    pub remote_cluster: Cluster,
    pub lifecycle: LifecycleMode,
    pub commit_compute_unit_price: u64,
    pub payer_init_lamports: Option<u64>,
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
    pub fn to_account_cloner_permissions(&self) -> AccountClonerPermissions {
        match self {
            LifecycleMode::Replica => AccountClonerPermissions {
                allow_cloning_refresh: false,
                allow_cloning_wallet_accounts: true,
                allow_cloning_data_accounts: true,
                allow_cloning_delegated_accounts: true,
                allow_cloning_program_accounts: true,
            },
            LifecycleMode::ProgramsReplica => AccountClonerPermissions {
                allow_cloning_refresh: false,
                allow_cloning_wallet_accounts: false,
                allow_cloning_data_accounts: false,
                allow_cloning_delegated_accounts: false,
                allow_cloning_program_accounts: true,
            },
            LifecycleMode::Ephemeral => AccountClonerPermissions {
                allow_cloning_refresh: true,
                allow_cloning_wallet_accounts: true,
                allow_cloning_data_accounts: true,
                allow_cloning_delegated_accounts: true,
                allow_cloning_program_accounts: true,
            },
            LifecycleMode::Offline => AccountClonerPermissions {
                allow_cloning_refresh: false,
                allow_cloning_wallet_accounts: false,
                allow_cloning_data_accounts: false,
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
