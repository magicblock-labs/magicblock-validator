use std::{
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

use sleipnir_accounts::{
    Cluster, ExternalConfig, ExternalReadonlyMode, ExternalWritableMode,
};
use solana_sdk::genesis_config::ClusterType;

pub fn timestamp_in_secs() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("create timestamp in timing");
    now.as_secs()
}

// mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev
pub const TEST_KEYPAIR_BYTES: [u8; 64] = [
    7, 83, 184, 55, 200, 223, 238, 137, 166, 244, 107, 126, 189, 16, 194, 36,
    228, 68, 43, 143, 13, 91, 3, 81, 53, 253, 26, 36, 50, 198, 40, 159, 11, 80,
    9, 208, 183, 189, 108, 200, 89, 77, 168, 76, 233, 197, 132, 22, 21, 186,
    202, 240, 105, 168, 157, 64, 233, 249, 100, 104, 210, 41, 83, 87,
];
// -----------------
// ExternalConfig from Sleipnir AccountsConfig
// -----------------
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
