use sleipnir_config::{
    AccountsConfig, CloneStrategy, ReadonlyMode, ValidatorConfig, WritableMode,
};

#[test]
fn test_empty_toml() {
    let toml = include_str!("fixtures/01_empty.toml");
    let config = toml::from_str::<ValidatorConfig>(toml).unwrap();

    assert_eq!(config, ValidatorConfig::default());
}

#[test]
fn test_defaults_toml() {
    let toml = include_str!("fixtures/02_defaults.toml");
    let config = toml::from_str::<ValidatorConfig>(toml).unwrap();
    assert_eq!(config, ValidatorConfig::default());
}

#[test]
fn test_local_dev_toml() {
    let toml = include_str!("fixtures/03_local-dev.toml");
    let config = toml::from_str::<ValidatorConfig>(toml).unwrap();
    assert_eq!(config, ValidatorConfig::default());
}

#[test]
fn test_ephemeral_toml() {
    let toml = include_str!("fixtures/04_ephemeral.toml");
    let config = toml::from_str::<ValidatorConfig>(toml).unwrap();
    assert_eq!(
        config,
        ValidatorConfig {
            accounts: AccountsConfig {
                clone: CloneStrategy {
                    readonly: ReadonlyMode::Programs,
                    writable: WritableMode::Delegated,
                },
                create: false,
                ..Default::default()
            },
        }
    );
}

#[test]
fn test_all_goes_toml() {
    let toml = include_str!("fixtures/05_all-goes.toml");
    let config = toml::from_str::<ValidatorConfig>(toml).unwrap();
    assert_eq!(
        config,
        ValidatorConfig {
            accounts: AccountsConfig {
                clone: CloneStrategy {
                    readonly: ReadonlyMode::All,
                    writable: WritableMode::All,
                },
                ..Default::default()
            },
        }
    );
}
