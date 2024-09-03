use sleipnir_accounts::{AccountsConfig, Cluster, LifecycleMode};
use sleipnir_config::errors::ConfigResult;
use solana_sdk::genesis_config::ClusterType;

pub(crate) fn try_convert_accounts_config(
    conf: &sleipnir_config::AccountsConfig,
) -> ConfigResult<AccountsConfig> {
    let remote_cluster = cluster_from_remote(&conf.remote);
    let lifecycle = lifecycle_mode_from_lifecycle_mode(&conf.lifecycle);
    let payer_init_lamports = conf.payer.try_init_lamports()?;
    Ok(AccountsConfig {
        remote_cluster,
        lifecycle,
        payer_init_lamports,
        commit_compute_unit_price: conf.commit.compute_unit_price,
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
        EphemeralLimited => LifecycleMode::EphemeralLimited,
        Ephemeral => LifecycleMode::Ephemeral,
        Offline => LifecycleMode::Offline,
    }
}
