use integration_test_tools::tmpdir::resolve_tmp_dir;
use integration_test_tools::validator::{
    resolve_workspace_dir, start_magic_block_validator_with_config,
    TestRunnerPaths,
};
use integration_test_tools::IntegrationTestContext;
use sleipnir_config::{AccountsConfig, SleipnirConfig};
use sleipnir_config::{LedgerConfig, LifecycleMode};
use solana_sdk::clock::Slot;
use std::path::Path;
use std::process::Child;
use std::{fs, process};
use tempfile::TempDir;

pub const TMP_DIR_LEDGER: &str = "TMP_DIR_LEDGER";
pub const TMP_DIR_CONFIG: &str = "TMP_DIR_CONFIG";
/// The minimum of slots we should wait for before shutting down a validator that
/// was writing the ledger.
pub const SLOT_WRITE_DELTA: Slot = 15;

/// Stringifies the config and writes it to a temporary config file.
/// Then uses that config to start the validator.
pub fn start_validator_with_config(
    config: SleipnirConfig,
) -> (TempDir, Option<process::Child>) {
    let workspace_dir = resolve_workspace_dir();
    let (default_tmpdir, temp_dir) = resolve_tmp_dir(TMP_DIR_CONFIG);
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
    (
        default_tmpdir,
        start_magic_block_validator_with_config(&paths, "TEST"),
    )
}

pub fn setup_offline_validator(
    ledger_path: &Path,
    reset: bool,
) -> (TempDir, Child, IntegrationTestContext) {
    let accounts_config = AccountsConfig {
        lifecycle: LifecycleMode::Offline,
        ..Default::default()
    };

    let config = SleipnirConfig {
        ledger: LedgerConfig {
            reset,
            path: Some(ledger_path.display().to_string()),
        },
        accounts: accounts_config.clone(),
        ..Default::default()
    };
    let (default_tmpdir_config, Some(validator)) =
        start_validator_with_config(config)
    else {
        panic!("validator should set up correctly");
    };

    let ctx = IntegrationTestContext::new_ephem_only();
    (default_tmpdir_config, validator, ctx)
}
