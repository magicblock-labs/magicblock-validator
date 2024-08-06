use std::error::Error;

use sleipnir_accounts::{AccountsConfig, CloneMode, Cluster, ExternalConfig};
use sleipnir_config::CloneMode;
use solana_sdk::genesis_config::ClusterType;

pub(crate) fn try_convert_accounts_config(
    conf: &AccountsConfig,
) -> Result<AccountsConfig, Box<dyn Error>> {
    let cluster = cluster_from_remote(&conf.remote);
    let clone = clone_mode_from_external(&conf.clone);
    let payer_init_lamports = conf.payer.try_init_lamports()?;

    let external = ExternalConfig { cluster, clone };

    Ok(AccountsConfig {
        external,
        create: conf.create,
        payer_init_lamports,
        commit_compute_unit_price: conf.commit.compute_unit_price,
    })
}

pub(crate) fn cluster_from_remote(remote: &RemoteConfig) -> Cluster {
    match remote {
        RemoteConfig::Devnet => Cluster::Known(ClusterType::Devnet),
        RemoteConfig::Mainnet => Cluster::Known(ClusterType::MainnetBeta),
        RemoteConfig::Testnet => Cluster::Known(ClusterType::Testnet),
        RemoteConfig::Development => Cluster::Known(ClusterType::Development),
        RemoteConfig::Custom(url) => Cluster::Custom(url.to_string()),
    }
}

fn clone_mode_from_external(clone: &CloneMode) -> CloneMode {
    match clone {
        CloneMode::Everything => CloneMode::Everything,
        CloneMode::ProgramsOnly => CloneMode::ProgramsOnly,
        CloneMode::Nothing => CloneMode::Nothing,
    }
}
