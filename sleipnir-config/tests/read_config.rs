use std::{
    env,
    net::{IpAddr, Ipv4Addr},
    str::FromStr,
};

use sleipnir_config::{
    AccountsConfig, CloneMode, CommitStrategy, GeyserGrpcConfig, ProgramConfig,
    RemoteConfig, RpcConfig, SleipnirConfig, ValidatorConfig,
};
use solana_sdk::pubkey::Pubkey;
use test_tools_core::paths::cargo_workspace_dir;
use url::Url;

#[test]
fn test_load_local_dev_with_programs_toml() {
    let workspace_dir = cargo_workspace_dir();
    let config_file_dir = workspace_dir
        .join("sleipnir-config")
        .join("tests")
        .join("fixtures")
        .join("06_local-dev-with-programs.toml");
    let config =
        SleipnirConfig::try_load_from_file(config_file_dir.to_str().unwrap())
            .unwrap();

    assert_eq!(
        config,
        SleipnirConfig {
            accounts: AccountsConfig {
                commit: CommitStrategy {
                    frequency_millis: 600_000,
                    compute_unit_price: 0,
                },
                ..Default::default()
            },
            programs: vec![ProgramConfig {
                id: Pubkey::from_str(
                    "wormH7q6y9EBUUL6EyptYhryxs6HoJg8sPK3LMfoNf4"
                )
                .unwrap(),
                path: format!(
                    "{}/../demos/magic-worm/target/deploy/program_solana.so",
                    config_file_dir.parent().unwrap().to_str().unwrap()
                )
            }],
            rpc: RpcConfig {
                addr: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                port: 7799,
            },
            geyser_grpc: GeyserGrpcConfig {
                addr: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                port: 11000,
            },
            validator: ValidatorConfig {
                millis_per_slot: 14,
                ..Default::default()
            },
        }
    )
}

#[test]
fn test_load_local_dev_with_programs_toml_envs_override() {
    let workspace_dir = cargo_workspace_dir();
    let config_file_dir = workspace_dir
        .join("sleipnir-config")
        .join("tests")
        .join("fixtures")
        .join("06_local-dev-with-programs.toml");

    // Values from the toml file should be overridden by the ENV variables
    let base_cluster = "http://remote-account-url";

    // Set the ENV variables
    env::set_var("ACCOUNTS_REMOTE", base_cluster);
    env::set_var("ACCOUNTS_CLONE", "all");
    env::set_var("ACCOUNTS_COMMIT_FREQUENCY_MILLIS", "123");
    env::set_var("ACCOUNTS_COMMIT_COMPUTE_UNIT_PRICE", "1");
    env::set_var("ACCOUNTS_CREATE", "false");
    env::set_var("RPC_ADDR", "0.1.0.1");
    env::set_var("RPC_PORT", "123");
    env::set_var("GEYSER_GRPC_ADDR", "0.1.0.1");
    env::set_var("GEYSER_GRPC_PORT", "123");
    env::set_var("VALIDATOR_MILLIS_PER_SLOT", "100");

    let config =
        SleipnirConfig::try_load_from_file(config_file_dir.to_str().unwrap())
            .unwrap();
    let config = config.override_from_envs();

    assert_eq!(
        config,
        SleipnirConfig {
            accounts: AccountsConfig {
                clone: CloneMode::Everything,
                commit: CommitStrategy {
                    frequency_millis: 123,
                    compute_unit_price: 1,
                },
                create: false,
                remote: RemoteConfig::Custom(Url::parse(base_cluster).unwrap()),
                ..Default::default()
            },
            programs: vec![ProgramConfig {
                id: Pubkey::from_str(
                    "wormH7q6y9EBUUL6EyptYhryxs6HoJg8sPK3LMfoNf4"
                )
                .unwrap(),
                path: format!(
                    "{}/../demos/magic-worm/target/deploy/program_solana.so",
                    config_file_dir.parent().unwrap().to_str().unwrap()
                )
            }],
            rpc: RpcConfig {
                addr: IpAddr::V4(Ipv4Addr::new(0, 1, 0, 1)),
                port: 123,
            },
            geyser_grpc: GeyserGrpcConfig {
                addr: IpAddr::V4(Ipv4Addr::new(0, 1, 0, 1)),
                port: 123,
            },
            validator: ValidatorConfig {
                millis_per_slot: 100,
                ..Default::default()
            },
        }
    )
}
