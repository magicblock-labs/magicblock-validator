use std::{
    fs,
    net::TcpStream,
    path::{Path, PathBuf},
    process::{self, Child},
    thread::sleep,
    time::Duration,
};

use magicblock_config::{
    EphemeralConfig, MetricsConfig, ProgramConfig, RpcConfig,
};
use random_port::{PortPicker, Protocol};
use tempfile::TempDir;

use crate::{
    loaded_accounts::LoadedAccounts,
    tmpdir::resolve_tmp_dir,
    toml_to_args::{config_to_args, rpc_port_from_config, ProgramLoader},
    workspace_paths::path_relative_to_workspace,
};

pub fn start_magic_block_validator_with_config(
    test_runner_paths: &TestRunnerPaths,
    log_suffix: &str,
    loaded_chain_accounts: &LoadedAccounts,
    release: bool,
) -> Option<Child> {
    let TestRunnerPaths {
        config_path,
        root_dir,
        ..
    } = test_runner_paths;

    let port = rpc_port_from_config(config_path);

    // First build so that the validator can start fast
    let mut command = process::Command::new("cargo");
    let keypair_base58 = loaded_chain_accounts.validator_authority_base58();
    command.arg("build");
    if release {
        command.arg("--release");
    }
    let build_res = command.current_dir(root_dir.clone()).output();

    if build_res.is_ok_and(|output| !output.status.success()) {
        eprintln!("Failed to build validator");
        return None;
    }

    // Start validator via `cargo run -- <path to config>`
    let mut command = process::Command::new("cargo");
    command.arg("run");
    if release {
        command.arg("--release");
    }
    let rust_log_style =
        std::env::var("RUST_LOG_STYLE").unwrap_or(log_suffix.to_string());
    command
        .arg("--")
        .arg(config_path)
        .env("RUST_LOG_STYLE", rust_log_style)
        .env("VALIDATOR_KEYPAIR", keypair_base58.clone())
        .current_dir(root_dir);

    eprintln!("Starting validator with {:?}", command);
    eprintln!(
        "Setting validator keypair to {} ({})",
        loaded_chain_accounts.validator_authority(),
        keypair_base58
    );

    let validator = command.spawn().expect("Failed to start validator");
    wait_for_validator(validator, port)
}

pub fn start_test_validator_with_config(
    test_runner_paths: &TestRunnerPaths,
    program_loader: Option<ProgramLoader>,
    loaded_accounts: &LoadedAccounts,
    log_suffix: &str,
) -> Option<process::Child> {
    let TestRunnerPaths {
        config_path,
        root_dir,
        workspace_dir,
    } = test_runner_paths;

    let port = rpc_port_from_config(config_path);
    let mut args = config_to_args(config_path, program_loader);

    let accounts_dir = workspace_dir.join("configs").join("accounts");
    let accounts = devnet_accounts(loaded_accounts);
    let resolved_extra_accounts =
        loaded_accounts.extra_accounts(workspace_dir, &accounts_dir);
    let accounts = accounts.iter().chain(&resolved_extra_accounts);

    let account_args = accounts
        .flat_map(|(account, file)| {
            let account_path = accounts_dir.join(file).canonicalize().unwrap();
            vec![
                "--account".to_string(),
                account.clone(),
                account_path.to_str().unwrap().to_string(),
            ]
        })
        .collect::<Vec<_>>();

    args.extend(account_args);

    let mut script = "#!/bin/bash\nsolana-test-validator".to_string();
    for arg in &args {
        script.push_str(&format!(" \\\n  {}", arg));
    }
    let mut command = process::Command::new("solana-test-validator");
    let rust_log_style =
        std::env::var("RUST_LOG_STYLE").unwrap_or(log_suffix.to_string());
    command
        .args(args)
        .env("RUST_LOG", "solana=warn")
        .env("RUST_LOG_STYLE", rust_log_style)
        .current_dir(root_dir);

    eprintln!("Starting test validator with {:?}", command);
    eprintln!("{}", script);
    let validator = command.spawn().expect("Failed to start validator");
    wait_for_validator(validator, port)
}

pub fn start_light_validator_with_config(
    test_runner_paths: &TestRunnerPaths,
    program_loader: Option<ProgramLoader>,
    loaded_accounts: &LoadedAccounts,
    log_suffix: &str,
) -> Option<process::Child> {
    let TestRunnerPaths {
        config_path,
        root_dir,
        workspace_dir,
    } = test_runner_paths;

    let port = rpc_port_from_config(config_path);
    let mut devnet_args = config_to_args(config_path, program_loader);

    // Remove args already set by light test-validator (and their values)
    let args_to_remove = [
        ("--rpc-port", true),
        ("--limit-ledger-size", true),
        ("--log", false),
        ("-r", false),
    ];
    let mut filtered_devnet_args = Vec::with_capacity(devnet_args.len());
    let mut skip_next = false;
    for arg in devnet_args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if let Some(&(_, has_value)) =
            args_to_remove.iter().find(|&&(flag, _)| flag == arg)
        {
            skip_next = has_value;
            continue;
        }
        filtered_devnet_args.push(arg);
    }
    devnet_args = filtered_devnet_args;

    // Add accounts to the validator args
    let accounts_dir = workspace_dir.join("configs").join("accounts");
    let accounts = devnet_accounts(loaded_accounts);
    let resolved_extra_accounts =
        loaded_accounts.extra_accounts(workspace_dir, &accounts_dir);
    let account_args = accounts
        .iter()
        .chain(&resolved_extra_accounts)
        .flat_map(|(account, file)| {
            let account_path = accounts_dir.join(file).canonicalize().unwrap();
            vec![
                "--account".to_string(),
                account.clone(),
                account_path.to_str().unwrap().to_string(),
            ]
        })
        .collect::<Vec<_>>();
    devnet_args.extend(account_args);

    // Split args using shlex so that the light CLI can pass them to the validator
    let validator_args = shlex::split(
        format!("--validator-args=\"{}\"", devnet_args.join(" ")).as_str(),
    )
    .ok_or_else(|| anyhow::anyhow!("invalid validator args"))
    .unwrap();

    let mut light_args = vec!["--rpc-port".to_string(), port.to_string()];
    light_args.extend(validator_args);

    let mut script = "#!/bin/bash\nlight test-validator".to_string();
    for arg in &light_args {
        script.push_str(&format!(" \\\n  {}", arg));
    }
    let mut command = process::Command::new("light");
    let rust_log_style =
        std::env::var("RUST_LOG_STYLE").unwrap_or(log_suffix.to_string());
    command
        .arg("test-validator")
        .args(light_args)
        .env("RUST_LOG", "solana=warn")
        .env("RUST_LOG_STYLE", rust_log_style)
        .current_dir(root_dir);

    eprintln!("Starting light validator with {:?}", command);
    eprintln!("{}", script);
    let validator = command.spawn().expect("Failed to start validator");
    // Waiting for the prover, which is the last thing to start
    wait_for_validator(validator, 3001)
}

pub fn wait_for_validator(mut validator: Child, port: u16) -> Option<Child> {
    const SLEEP_DURATION: Duration = Duration::from_millis(400);
    let max_retries = if std::env::var("CI").is_ok() {
        1500
    } else {
        800
    };

    for _ in 0..max_retries {
        if TcpStream::connect(format!("0.0.0.0:{}", port)).is_ok() {
            return Some(validator);
        }

        sleep(SLEEP_DURATION);
    }

    eprintln!(
        "Validator RPC on port {} failed to listen after {:.1} seconds",
        port,
        max_retries as f32 * SLEEP_DURATION.as_secs_f32()
    );
    validator.kill().expect("Failed to kill validator");
    None
}

pub const TMP_DIR_CONFIG: &str = "TMP_DIR_CONFIG";

fn resolve_port() -> u16 {
    std::env::var("EPHEM_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| {
            PortPicker::new()
                .random(true)
                .protocol(Protocol::Tcp)
                .pick()
                .unwrap()
        })
}

/// Stringifies the config and writes it to a temporary config file.
/// Sets the RPC port to a random available port to allow multiple tests to
/// run in parallel.
/// Then uses that config to start the validator.
pub fn start_magicblock_validator_with_config_struct(
    config: EphemeralConfig,
    loaded_chain_accounts: &LoadedAccounts,
) -> (TempDir, Option<process::Child>, u16) {
    let port = resolve_port();
    let config = EphemeralConfig {
        rpc: RpcConfig {
            port,
            ..config.rpc.clone()
        },
        metrics: MetricsConfig {
            enabled: false,
            ..config.metrics.clone()
        },
        ..config.clone()
    };
    let workspace_dir = resolve_workspace_dir();
    let (default_tmpdir, temp_dir) = resolve_tmp_dir(TMP_DIR_CONFIG);
    let release = std::env::var("RELEASE").is_ok();
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
        start_magic_block_validator_with_config(
            &paths,
            "TEST",
            loaded_chain_accounts,
            release,
        ),
        port,
    )
}

pub fn start_magicblock_validator_with_config_struct_and_temp_dir(
    config: EphemeralConfig,
    loaded_chain_accounts: &LoadedAccounts,
    default_tmpdir: TempDir,
    temp_dir: PathBuf,
) -> (TempDir, Option<process::Child>, u16) {
    let port = resolve_port();
    let config = EphemeralConfig {
        rpc: RpcConfig {
            port,
            ..config.rpc.clone()
        },
        metrics: MetricsConfig {
            enabled: false,
            ..config.metrics.clone()
        },
        ..config.clone()
    };

    let workspace_dir = resolve_workspace_dir();
    let release = std::env::var("RELEASE").is_ok();
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
        start_magic_block_validator_with_config(
            &paths,
            "TEST",
            loaded_chain_accounts,
            release,
        ),
        port,
    )
}

pub fn cleanup(validator: &mut Child) {
    let _ = validator.kill().inspect_err(|e| {
        eprintln!("ERR: Failed to kill validator: {:?}", e);
    });
}

/// Directories
pub struct TestRunnerPaths {
    pub config_path: PathBuf,
    pub root_dir: PathBuf,
    pub workspace_dir: PathBuf,
}

pub fn resolve_workspace_dir() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    Path::new(&manifest_dir)
        .join("..")
        .canonicalize()
        .unwrap()
        .to_path_buf()
}

pub fn resolve_programs(
    programs: Option<Vec<ProgramConfig>>,
) -> Vec<ProgramConfig> {
    programs
        .map(|programs| {
            programs
                .into_iter()
                .map(|program| ProgramConfig {
                    id: program.id,
                    path: path_relative_to_workspace(&format!(
                        "target/deploy/{}",
                        program.path
                    )),
                })
                .collect()
        })
        .unwrap_or_default()
}

// -----------------
// Utilities
// -----------------

fn devnet_accounts(loaded_accounts: &LoadedAccounts) -> [(String, String); 8] {
    [
        (
            loaded_accounts.validator_authority().to_string(),
            "validator-authority.json".to_string(),
        ),
        (
            loaded_accounts.luzid_authority().to_string(),
            "luzid-authority.json".to_string(),
        ),
        (
            loaded_accounts.validator_fees_vault().to_string(),
            "validator-fees-vault.json".to_string(),
        ),
        (
            loaded_accounts.protocol_fees_vault().to_string(),
            "protocol-fees-vault.json".to_string(),
        ),
        (
            "9yXjZTevvMp1XgZSZEaziPRgFiXtAQChpnP2oX9eCpvt".to_string(),
            "non-delegated-cloneable-account1.json".to_string(),
        ),
        (
            "BHBuATGifAD4JbRpM5nVdyhKzPgv3p2CxLEHAqwBzAj5".to_string(),
            "non-delegated-cloneable-account2.json".to_string(),
        ),
        (
            "2o48ieM95rmHqMWC5B3tTX4DL7cLm4m1Kuwjay3keQSv".to_string(),
            "non-delegated-cloneable-account3.json".to_string(),
        ),
        (
            "2EmfL3MqL3YHABudGNmajjCpR13NNEn9Y4LWxbDm6SwR".to_string(),
            "non-delegated-cloneable-account4.json".to_string(),
        ),
    ]
}

/// Unwraps the provided result and ensures to kill the validator before panicking
/// if the result was an error
#[macro_export]
macro_rules! expect {
    ($res:expr, $msg:expr, $validator:ident) => {
        match $res {
            Ok(val) => val,
            Err(e) => {
                $validator.kill().unwrap();
                panic!("{}: {:?}", $msg, e);
            }
        }
    };
    ($res:expr, $validator:ident) => {
        match $res {
            Ok(val) => val,
            Err(e) => {
                $validator.kill().unwrap();
                panic!("{:?}", e);
            }
        }
    };
}

/// Unwraps the provided result and ensures to kill the validator before panicking
/// if the result was not an error
#[macro_export]
macro_rules! expect_err {
    ($res:expr, $msg:expr, $validator:ident) => {
        match $res {
            Ok(_) => {
                $validator.kill().unwrap();
                panic!("{}", $msg);
            }
            Err(e) => e,
        }
    };
    ($res:expr, $validator:ident) => {
        match $res {
            Ok(_) => {
                $validator.kill().unwrap();
                panic!("Expected Error");
            }
            Err(e) => e,
        }
    };
}

/// Unwraps the provided option and ensures to kill the validator before panicking
/// if the result wasi None
#[macro_export]
macro_rules! unwrap {
    ($res:expr, $msg:expr, $validator:ident) => {
        match $res {
            Some(val) => val,
            None => {
                $validator.kill().unwrap();
                panic!("{}", $msg);
            }
        }
    };
    ($res:expr, $validator:ident) => {
        match $res {
            Some(val) => val,
            None => {
                $validator.kill().unwrap();
                panic!("Failed to unwrap");
            }
        }
    };
}
