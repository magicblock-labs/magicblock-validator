use sleipnir_accounts::{
    Cluster, ExternalConfig, ExternalReadonlyMode, ExternalWritableMode,
};
use solana_sdk::genesis_config::ClusterType;
use std::error::Error;

pub fn try_convert_accounts_config(
    conf: &sleipnir_config::AccountsConfig,
) -> Result<sleipnir_accounts::AccountsConfig, Box<dyn Error>> {
    let cluster = cluster_from_remote(&conf.remote);
    let readonly = readonly_mode_from_external(&conf.clone.readonly);
    let writable = writable_mode_from_external(&conf.clone.writable);
    let payer_init_lamports = conf.payer.try_init_lamports()?;

    let external = ExternalConfig {
        cluster,
        readonly,
        writable,
    };

    Ok(sleipnir_accounts::AccountsConfig {
        external,
        create: conf.create,
        payer_init_lamports,
        commit_compute_unit_price: conf.commit.compute_unit_price,
    })
}

fn cluster_from_remote(
    remote: &sleipnir_config::RemoteConfig,
) -> sleipnir_accounts::Cluster {
    use sleipnir_config::RemoteConfig::*;
    match remote {
        Devnet => Cluster::Known(ClusterType::Devnet),
        Mainnet => Cluster::Known(ClusterType::MainnetBeta),
        Testnet => Cluster::Known(ClusterType::Testnet),
        Development => Cluster::Known(ClusterType::Development),
        Custom(url) => Cluster::Custom(url.to_string()),
    }
}

fn readonly_mode_from_external(
    mode: &sleipnir_config::ReadonlyMode,
) -> sleipnir_accounts::ExternalReadonlyMode {
    use sleipnir_config::ReadonlyMode::*;
    match mode {
        All => ExternalReadonlyMode::All,
        Programs => ExternalReadonlyMode::Programs,
        None => ExternalReadonlyMode::None,
    }
}

fn writable_mode_from_external(
    mode: &sleipnir_config::WritableMode,
) -> sleipnir_accounts::ExternalWritableMode {
    use sleipnir_config::WritableMode::*;
    match mode {
        All => ExternalWritableMode::All,
        Delegated => ExternalWritableMode::Delegated,
        None => ExternalWritableMode::None,
    }
}
