use std::net::{IpAddr, Ipv4Addr};

use magicblock_config::{
    AccountsConfig, AllowedProgram, CommitStrategy, GeyserGrpcConfig,
    LedgerConfig, LifecycleMode, MetricsConfig, MetricsServiceConfig, Payer,
    ProgramConfig, RemoteConfig, RpcConfig, SleipnirConfig, ValidatorConfig,
};
use solana_sdk::{native_token::LAMPORTS_PER_SOL, pubkey};
use url::Url;

#[test]
fn test_empty_toml() {
    let toml = include_str!("fixtures/01_empty.toml");
    let config = toml::from_str::<SleipnirConfig>(toml).unwrap();

    assert_eq!(config, SleipnirConfig::default());
}

#[test]
fn test_defaults_toml() {
    let toml = include_str!("fixtures/02_defaults.toml");
    let config = toml::from_str::<SleipnirConfig>(toml).unwrap();
    assert_eq!(config, SleipnirConfig::default());
}

#[test]
fn test_local_dev_toml() {
    let toml = include_str!("fixtures/03_local-dev.toml");
    let config = toml::from_str::<SleipnirConfig>(toml).unwrap();
    assert_eq!(config, SleipnirConfig::default());
}

#[test]
fn test_ephemeral_toml() {
    let toml = include_str!("fixtures/04_ephemeral.toml");
    let config = toml::from_str::<SleipnirConfig>(toml).unwrap();
    assert_eq!(
        config,
        SleipnirConfig {
            accounts: AccountsConfig {
                lifecycle: LifecycleMode::Ephemeral,
                allowed_programs: vec![AllowedProgram {
                    id: pubkey!("wormH7q6y9EBUUL6EyptYhryxs6HoJg8sPK3LMfoNf4")
                }],
                ..Default::default()
            },
            ..Default::default()
        }
    );
}

#[test]
fn test_all_goes_toml() {
    let toml = include_str!("fixtures/05_all-goes.toml");
    let config = toml::from_str::<SleipnirConfig>(toml).unwrap();
    assert_eq!(
        config,
        SleipnirConfig {
            accounts: AccountsConfig {
                lifecycle: LifecycleMode::Replica,
                ..Default::default()
            },
            validator: ValidatorConfig {
                sigverify: false,
                ..Default::default()
            },
            ledger: LedgerConfig {
                reset: false,
                ..Default::default()
            },
            ..Default::default()
        }
    );
}

#[test]
fn test_local_dev_with_programs_toml() {
    let toml = include_str!("fixtures/06_local-dev-with-programs.toml");
    let config = toml::from_str::<SleipnirConfig>(toml).unwrap();

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
                id: pubkey!("wormH7q6y9EBUUL6EyptYhryxs6HoJg8sPK3LMfoNf4"),
                path: "../demos/magic-worm/target/deploy/program_solana.so"
                    .to_string(),
            }],
            rpc: RpcConfig {
                addr: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                port: 7799
            },
            validator: ValidatorConfig {
                millis_per_slot: 14,
                ..Default::default()
            },
            ledger: LedgerConfig {
                ..Default::default()
            },
            geyser_grpc: GeyserGrpcConfig {
                addr: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                port: 11_000
            },
            metrics: MetricsConfig {
                enabled: true,
                service: MetricsServiceConfig {
                    port: 9999,
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    )
}

#[test]
fn test_custom_remote_toml() {
    let toml = include_str!("fixtures/07_custom-remote.toml");
    let config = toml::from_str::<SleipnirConfig>(toml).unwrap();

    assert_eq!(
        config,
        SleipnirConfig {
            accounts: AccountsConfig {
                remote: RemoteConfig::Custom(
                    Url::parse("http://localhost:8899").unwrap()
                ),
                ..Default::default()
            },
            ..Default::default()
        }
    );
}

#[test]
fn test_custom_ws_remote_toml() {
    let toml = include_str!("fixtures/09_custom-ws-remote.toml");
    let config = toml::from_str::<SleipnirConfig>(toml).unwrap();

    assert_eq!(
        config,
        SleipnirConfig {
            accounts: AccountsConfig {
                remote: RemoteConfig::CustomWithWs(
                    Url::parse("http://localhost:8899").unwrap(),
                    Url::parse("ws://localhost:9001").unwrap()
                ),
                ..Default::default()
            },
            ..Default::default()
        }
    );
}

#[test]
fn test_accounts_payer() {
    let toml = include_str!("fixtures/08_accounts-payer.toml");
    let config = toml::from_str::<SleipnirConfig>(toml).unwrap();
    assert_eq!(
        config,
        SleipnirConfig {
            accounts: AccountsConfig {
                payer: Payer::new(None, Some(2_000)),
                ..Default::default()
            },
            ..Default::default()
        }
    );
    assert_eq!(
        config.accounts.payer.try_init_lamports().unwrap(),
        Some(2_000 * LAMPORTS_PER_SOL)
    );
}

#[test]
fn test_custom_invalid_remote() {
    let toml = r#"
[accounts]
remote = "http://localhost::8899"
"#;

    let res = toml::from_str::<SleipnirConfig>(toml);
    assert!(res.is_err());
}

#[test]
fn test_program_invalid_pubkey() {
    let toml = r#"
[[program]]
id = "not a pubkey"
path = "/tmp/program.so"
"#;

    let res = toml::from_str::<SleipnirConfig>(toml);
    eprintln!("{:?}", res);
    assert!(res.is_err());
}

#[test]
fn test_accounts_payer_specifies_both_lamports_and_sol() {
    let toml = r#"
[accounts]
payer = { init_sol = 2000, init_lamports = 300_000 }
"#;

    let config = toml::from_str::<SleipnirConfig>(toml).unwrap();
    assert!(config.accounts.payer.try_init_lamports().is_err());
}
