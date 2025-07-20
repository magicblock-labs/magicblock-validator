use std::collections::HashSet;

use magicblock_accounts::{AccountsConfig, Cluster, LifecycleMode};
use magicblock_config::errors::ConfigResult;
use solana_sdk::{genesis_config::ClusterType, pubkey::Pubkey};

pub(crate) fn try_convert_accounts_config(
    conf: &magicblock_config::AccountsConfig,
) -> ConfigResult<AccountsConfig> {
    Ok(AccountsConfig {
        remote_cluster: cluster_from_remote(&conf.remote),
        lifecycle: lifecycle_mode_from_lifecycle_mode(&conf.lifecycle),
        commit_compute_unit_price: conf.commit.compute_unit_price,
        allowed_program_ids: allowed_program_ids_from_allowed_programs(
            &conf.allowed_programs,
        ),
    })
}
pub(crate) fn cluster_from_remote(
    remote: &magicblock_config::RemoteConfig,
) -> Cluster {
    use magicblock_config::RemoteCluster::*;

    match remote.cluster {
        Devnet => Cluster::Known(ClusterType::Devnet),
        Mainnet => Cluster::Known(ClusterType::MainnetBeta),
        Testnet => Cluster::Known(ClusterType::Testnet),
        Development => Cluster::Known(ClusterType::Development),
        Custom => Cluster::Custom(
            remote.url.clone().expect("Custom remote must have a url"),
        ),
        CustomWithWs => Cluster::CustomWithWs(
            remote
                .url
                .clone()
                .expect("CustomWithWs remote must have a url"),
            remote
                .ws_url
                .clone()
                .expect("CustomWithWs remote must have a ws_url")
                .first()
                .expect("CustomWithWs remote must have at least one ws_url")
                .clone(),
        ),
        CustomWithMultipleWs => Cluster::CustomWithMultipleWs {
            http: remote
                .url
                .clone()
                .expect("CustomWithMultipleWs remote must have a url"),
            ws: remote
                .ws_url
                .clone()
                .expect("CustomWithMultipleWs remote must have a ws_url"),
        },
    }
}

fn lifecycle_mode_from_lifecycle_mode(
    clone: &magicblock_config::LifecycleMode,
) -> LifecycleMode {
    use magicblock_config::LifecycleMode::*;
    match clone {
        ProgramsReplica => LifecycleMode::ProgramsReplica,
        Replica => LifecycleMode::Replica,
        Ephemeral => LifecycleMode::Ephemeral,
        Offline => LifecycleMode::Offline,
    }
}

fn allowed_program_ids_from_allowed_programs(
    allowed_programs: &[magicblock_config::AllowedProgram],
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
