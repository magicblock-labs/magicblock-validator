use std::collections::HashSet;

use sleipnir_accounts::{AccountsConfig, Cluster, LifecycleMode};
use sleipnir_config::errors::ConfigResult;
use solana_sdk::{genesis_config::ClusterType, pubkey::Pubkey};

pub(crate) fn try_convert_accounts_config(
    conf: &sleipnir_config::AccountsConfig,
) -> ConfigResult<AccountsConfig> {
    let remote_cluster = cluster_from_remote(&conf.remote);
    let lifecycle = lifecycle_mode_from_lifecycle_mode(&conf.lifecycle);
    let commit_compute_unit_price = conf.commit.compute_unit_price;
    let payer_init_lamports = conf.payer.try_init_lamports()?;
    let allowed_program_ids =
        allowed_program_ids_from_allowed_programs(&conf.allowed_programs);
    Ok(AccountsConfig {
        remote_cluster,
        lifecycle,
        commit_compute_unit_price,
        payer_init_lamports,
        allowed_program_ids,
    })
}

fn cluster_from_remote(remote: &sleipnir_config::RemoteConfig) -> Cluster {
    use sleipnir_config::RemoteConfig::*;
    match remote {
        Devnet => Cluster::Known(ClusterType::Devnet),
        Mainnet => Cluster::Known(ClusterType::MainnetBeta),
        Testnet => Cluster::Known(ClusterType::Testnet),
        Development => Cluster::Known(ClusterType::Development),
        Custom(url) => Cluster::Custom(url.to_string()),
    }
}

fn lifecycle_mode_from_lifecycle_mode(
    clone: &sleipnir_config::LifecycleMode,
) -> LifecycleMode {
    use sleipnir_config::LifecycleMode::*;
    match clone {
        ProgramsReplica => LifecycleMode::ProgramsReplica,
        Replica => LifecycleMode::Replica,
        Ephemeral => LifecycleMode::Ephemeral,
        Offline => LifecycleMode::Offline,
    }
}

fn allowed_program_ids_from_allowed_programs(
    allowed_programs: &[sleipnir_config::AllowedProgram],
) -> Option<HashSet<Pubkey>> {
    if !allowed_programs.is_empty() {
        Some(HashSet::from_iter(
            allowed_programs
                .iter()
                .map(|allowed_program| allowed_program.id),
        ))
    } else {
        None
    }
}
