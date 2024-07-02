use std::{
    env,
    net::{IpAddr, Ipv4Addr},
    str::FromStr,
};

use sleipnir_config::{
    AccountsConfig, CloneStrategy, CommitStrategy, ProgramConfig, ReadonlyMode,
    RemoteConfig, RpcConfig, SleipnirConfig, ValidatorConfig, WritableMode,
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
                    trigger: false,
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
    env::set_var("ACCOUNTS.REMOTE", base_cluster);
    env::set_var("ACCOUNTS.CLONE.READONLY", "all");
    env::set_var("ACCOUNTS.CLONE.WRITABLE", "delegated");
    env::set_var("ACCOUNTS.COMMIT.FREQUENCY_MILLIS", "123");
    env::set_var("ACCOUNTS.COMMIT.TRIGGER", "true");
    env::set_var("ACCOUNTS.COMMIT.COMPUTE_UNIT_PRICE", "1");
    env::set_var("ACCOUNTS.CREATE", "false");
    env::set_var("RPC.ADDR", "0.1.0.1");
    env::set_var("RPC.PORT", "123");
    env::set_var("VALIDATOR.MILLIS_PER_SLOT", "100");

    let config =
        SleipnirConfig::try_load_from_file(config_file_dir.to_str().unwrap())
            .unwrap();
    let config = config.override_from_envs();

    assert_eq!(
        config,
        SleipnirConfig {
            accounts: AccountsConfig {
                clone: CloneStrategy {
                    readonly: ReadonlyMode::All,
                    writable: WritableMode::Delegated,
                },
                commit: CommitStrategy {
                    frequency_millis: 123,
                    trigger: true,
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
            validator: ValidatorConfig {
                millis_per_slot: 100,
                ..Default::default()
            },
        }
    )
}
