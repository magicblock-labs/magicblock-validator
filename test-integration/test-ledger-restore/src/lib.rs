use integration_test_tools::tmpdir::resolve_tmp_dir;
use integration_test_tools::validator::{
    resolve_workspace_dir, start_magic_block_validator_with_config,
    TestRunnerPaths,
};
use sleipnir_config::SleipnirConfig;
use std::path::Path;
use std::{fs, process};

/// Stringifies the config and writes it to a temporary config file.
/// Then uses that config to start the validator.
pub fn start_validator_with_config(
    config: SleipnirConfig,
) -> Option<process::Child> {
    let workspace_dir = resolve_workspace_dir();
    let (_, temp_dir) = resolve_tmp_dir("TMP_DIR_CONFIG");
    let config_path = temp_dir.join("config.toml");
    let config_toml = config.to_string();
    fs::write(&config_path, config_toml).unwrap();

    let root_dir = Path::new(&workspace_dir)
        .join("..")
        .canonicalize()
        .unwrap()
        .to_path_buf();
    let paths = TestRunnerPaths {
        config_path,
        root_dir,
        workspace_dir,
    };
    start_magic_block_validator_with_config(&paths, "TEST")
}
