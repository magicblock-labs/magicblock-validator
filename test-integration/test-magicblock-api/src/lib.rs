use std::{path::PathBuf, process::Child};

use integration_test_tools::{
    loaded_accounts::LoadedAccounts,
    validator::{
        start_magic_block_validator_with_config,
        start_test_validator_with_config, TestRunnerPaths,
    },
};

pub fn start_devnet_validator_with_config(config_name: &str) -> Child {
    let manifest_dir_raw = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_dir = PathBuf::from(&manifest_dir_raw);

    let config_path = manifest_dir.join("../configs/").join(config_name);
    let workspace_dir = manifest_dir.join("../");
    let root_dir = workspace_dir.join("../");
    let test_paths = TestRunnerPaths {
        config_path,
        root_dir,
        workspace_dir,
    };
    match start_test_validator_with_config(
        &test_paths,
        None,
        &Default::default(),
        "CHAIN",
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start ephemeral validator properly");
        }
    }
}

pub fn start_magicblock_validator_with_config(
    config_name: &str,
    loaded_accounts: &LoadedAccounts,
) -> Child {
    let manifest_dir_raw = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_dir = PathBuf::from(&manifest_dir_raw);

    let config_path = manifest_dir.join("../configs/").join(config_name);
    let workspace_dir = manifest_dir.join("../");
    let root_dir = workspace_dir.join("../");
    let test_paths = TestRunnerPaths {
        config_path,
        root_dir,
        workspace_dir,
    };
    match start_magic_block_validator_with_config(
        &test_paths,
        "EPHEM",
        loaded_accounts,
        true,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start ephemeral validator properly");
        }
    }
}

pub fn cleanup(validator: &mut Child) {
    let _ = validator.kill().inspect_err(|e| {
        eprintln!("ERR: Failed to kill validator: {:?}", e);
    });
}
