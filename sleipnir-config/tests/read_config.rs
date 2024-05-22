use sleipnir_config::{
    ProgramConfig, RpcConfig, SleipnirConfig, ValidatorConfig,
};
use test_tools_core::paths::cargo_workspace_dir;

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
            programs: vec![ProgramConfig {
                id: "wormH7q6y9EBUUL6EyptYhryxs6HoJg8sPK3LMfoNf4".to_string(),
                path: format!(
                    "{}/../demos/magic-worm/target/deploy/program_solana.so",
                    config_file_dir.parent().unwrap().to_str().unwrap()
                )
            }],
            rpc: RpcConfig { port: 7799 },
            validator: ValidatorConfig {
                millis_per_slot: 14
            },
            ..SleipnirConfig::default()
        }
    )
}
