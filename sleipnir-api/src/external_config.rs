use std::error::Error;

use sleipnir_accounts::{Cluster, ExternalCloneMode};
use solana_sdk::genesis_config::ClusterType;

pub(crate) fn try_convert_accounts_config(
    conf: &sleipnir_config::AccountsConfig,
) -> Result<sleipnir_accounts::AccountsConfig, Box<dyn Error>> {
    let cluster = cluster_from_remote(&conf.remote);
    let clone = external_clone_mode_from_clone_mode(&conf.clone);
    let payer_init_lamports = conf.payer.try_init_lamports()?;

    let external = sleipnir_accounts::ExternalConfig { cluster, clone };

    Ok(sleipnir_accounts::AccountsConfig {
        external,
        create: conf.create,
        payer_init_lamports,
        commit_compute_unit_price: conf.commit.compute_unit_price,
    })
}

pub(crate) fn cluster_from_remote(
    remote: &sleipnir_config::RemoteConfig,
) -> Cluster {
    use sleipnir_config::RemoteConfig::*;
    match remote {
        Devnet => Cluster::Known(ClusterType::Devnet),
        Mainnet => Cluster::Known(ClusterType::MainnetBeta),
        Testnet => Cluster::Known(ClusterType::Testnet),
        Development => Cluster::Known(ClusterType::Development),
        Custom(url) => Cluster::Custom(url.to_string()),
    }
}

fn external_clone_mode_from_clone_mode(
    clone: &sleipnir_config::CloneMode,
) -> ExternalCloneMode {
    use sleipnir_config::CloneMode::*;
    match clone {
        Everything => ExternalCloneMode::Everything,
        ProgramsOnly => ExternalCloneMode::ProgramsOnly,
        Nothing => ExternalCloneMode::Nothing,
    }
}
