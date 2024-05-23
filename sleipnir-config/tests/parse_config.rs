use sleipnir_config::{
    AccountsConfig, CloneStrategy, ProgramConfig, ReadonlyMode, RemoteConfig,
    RpcConfig, SleipnirConfig, ValidatorConfig, WritableMode,
};
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
                clone: CloneStrategy {
                    readonly: ReadonlyMode::Programs,
                    writable: WritableMode::Delegated,
                },
                create: false,
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
                clone: CloneStrategy {
                    readonly: ReadonlyMode::All,
                    writable: WritableMode::All,
                },
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
            programs: vec![ProgramConfig {
                id: "wormH7q6y9EBUUL6EyptYhryxs6HoJg8sPK3LMfoNf4".to_string(),
                path: "../demos/magic-worm/target/deploy/program_solana.so"
                    .to_string(),
            }],
            rpc: RpcConfig { port: 7799 },
            validator: ValidatorConfig {
                millis_per_slot: 14
            },
            ..SleipnirConfig::default()
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
fn test_custom_invalid_remote() {
    let toml = r#"
[accounts]
remote = "http://localhost::8899"
"#;

    let res = toml::from_str::<SleipnirConfig>(toml);
    assert!(res.is_err());
}
