use std::{
    fs,
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{self, Child},
    thread::sleep,
    time::Duration,
};

use magicblock_config::{
    config::LoadableProgram, types::BindAddress, ValidatorParams,
};
use rand::{thread_rng, Rng};
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use tempfile::TempDir;

use crate::{
    loaded_accounts::LoadedAccounts,
    tmpdir::resolve_tmp_dir,
    toml_to_args::{
        config_to_args, program_ids_from_config, rpc_port_from_config,
        ProgramLoader,
    },
    workspace_paths::path_relative_to_workspace,
};

pub fn start_magic_block_validator_with_config(
    test_runner_paths: &TestRunnerPaths,
    log_suffix: &str,
    loaded_chain_accounts: &LoadedAccounts,
) -> Option<Child> {
    let TestRunnerPaths {
        config_path,
        root_dir,
        ..
    } = test_runner_paths;

    let port = rpc_port_from_config(config_path);
    let keypair_base58 = loaded_chain_accounts.validator_authority_base58();

    // In CI we ship a prebuilt validator binary as an artifact so we never
    // pay for `cargo build` here. Locally we keep the convenient
    // `cargo build` + `cargo run` flow so source changes are picked up.
    let prebuilt =
        std::env::var("MAGICBLOCK_VALIDATOR_BIN").ok().filter(|p| {
            let exists = Path::new(p).is_file();
            if !exists {
                eprintln!(
                    "MAGICBLOCK_VALIDATOR_BIN={} does not exist, falling back to cargo run",
                    p
                );
            }
            exists
        });

    let mut command = if let Some(bin) = prebuilt.as_deref() {
        let mut c = process::Command::new(bin);
        c.arg(config_path);
        c
    } else {
        let mut build_cmd = process::Command::new("cargo");
        build_cmd.arg("build");
        let build_res = build_cmd.current_dir(root_dir.clone()).output();
        if build_res.is_ok_and(|output| !output.status.success()) {
            eprintln!("Failed to build validator");
            return None;
        }

        let mut c = process::Command::new("cargo");
        c.arg("run").arg("--").arg(config_path);
        c
    };

    let rust_log_style =
        std::env::var("RUST_LOG_STYLE").unwrap_or(log_suffix.to_string());
    command
        .env("RUST_LOG_STYLE", rust_log_style)
        .env("MBV_VALIDATOR__KEYPAIR", keypair_base58.clone())
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
    let accounts = [
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
            loaded_accounts.magic_fee_vault().to_string(),
            "magic-fee-vault.json".to_string(),
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
    ];
    let resolved_extra_accounts =
        loaded_accounts.extra_accounts(workspace_dir, &accounts_dir);
    let readiness_pubkeys = program_ids_from_config(config_path)
        .into_iter()
        .chain(
            accounts
                .iter()
                .chain(&resolved_extra_accounts)
                .map(|(account, _)| account.clone()),
        )
        .filter_map(|pubkey| {
            pubkey.parse::<Pubkey>().map_or_else(
                |err| {
                    eprintln!(
                        "Skipping invalid readiness pubkey {}: {:?}",
                        pubkey, err
                    );
                    None
                },
                Some,
            )
        })
        .collect::<Vec<_>>();

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
    let mut validator = wait_for_validator(validator, port)?;
    wait_for_required_accounts(&mut validator, port, &readiness_pubkeys)
        .then_some(validator)
}

pub fn wait_for_validator(mut validator: Child, port: u16) -> Option<Child> {
    const SLEEP_DURATION: Duration = Duration::from_millis(400);
    let max_retries = validator_startup_max_retries();

    for _ in 0..max_retries {
        if TcpStream::connect(format!("0.0.0.0:{}", port)).is_ok() {
            return Some(validator);
        }

        if let Some(status) = validator
            .try_wait()
            .expect("Failed to poll validator process")
        {
            eprintln!(
                "Validator RPC on port {} never listened; process exited early with {}",
                port, status
            );
            return None;
        }

        sleep(SLEEP_DURATION);
    }

    eprintln!(
        "Validator RPC on port {} failed to listen after {:.1} seconds",
        port,
        max_retries as f32 * SLEEP_DURATION.as_secs_f32()
    );
    validator.kill().expect("Failed to kill validator");
    validator.wait().expect("Failed to reap validator");
    None
}

fn wait_for_required_accounts(
    validator: &mut Child,
    port: u16,
    pubkeys: &[Pubkey],
) -> bool {
    if pubkeys.is_empty() {
        return true;
    }

    const SLEEP_DURATION: Duration = Duration::from_millis(400);
    let max_retries = validator_startup_max_retries();
    let rpc_client = RpcClient::new_with_commitment(
        format!("http://127.0.0.1:{}", port),
        CommitmentConfig::processed(),
    );

    for _ in 0..max_retries {
        if let Ok(accounts) = rpc_client.get_multiple_accounts(pubkeys) {
            if accounts.iter().all(Option::is_some) {
                return true;
            }
        }

        if let Some(status) = validator
            .try_wait()
            .expect("Failed to poll validator process")
        {
            eprintln!(
                "Validator RPC on port {} listened, but required accounts never became ready; process exited with {}",
                port, status
            );
            return false;
        }

        sleep(SLEEP_DURATION);
    }

    eprintln!(
        "Required validator accounts on port {} failed to become ready after {:.1} seconds",
        port,
        max_retries as f32 * SLEEP_DURATION.as_secs_f32()
    );
    validator.kill().expect("Failed to kill validator");
    validator.wait().expect("Failed to reap validator");
    false
}

fn validator_startup_max_retries() -> usize {
    if std::env::var("CI").is_ok() {
        1500
    } else {
        800
    }
}

pub const TMP_DIR_CONFIG: &str = "TMP_DIR_CONFIG";

const MIN_RANDOM_PORT: u16 = 1024;
const MAX_RANDOM_PORT: u16 = u16::MAX;
const RANDOM_PORT_ATTEMPTS: usize = 128;

fn resolve_port(bind_ip: IpAddr, exclude: &[u16]) -> u16 {
    std::env::var("EPHEM_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| {
            let mut rng = thread_rng();

            for _ in 0..RANDOM_PORT_ATTEMPTS {
                let port = rng.gen_range(MIN_RANDOM_PORT..=MAX_RANDOM_PORT);
                if exclude.contains(&port) {
                    continue;
                }

                if TcpListener::bind(SocketAddr::new(bind_ip, port)).is_ok() {
                    return port;
                }
            }

            loop {
                let port = TcpListener::bind(SocketAddr::new(bind_ip, 0))
                    .and_then(|listener| listener.local_addr())
                    .map(|addr| addr.port())
                    .unwrap();

                if !exclude.contains(&port) {
                    return port;
                }
            }
        })
}

/// Stringifies the config and writes it to a temporary config file.
/// Sets the RPC port to a random available port to allow multiple tests to
/// run in parallel.
/// Then uses that config to start the validator.
pub fn start_magicblock_validator_with_config_struct(
    config: ValidatorParams,
    loaded_chain_accounts: &LoadedAccounts,
) -> (TempDir, Option<process::Child>, u16) {
    let rpc_port = resolve_port(config.aperture.listen.ip(), &[]);
    let metrics_port = resolve_port(config.metrics.address.ip(), &[rpc_port]);

    let mut config = config.clone();
    config.aperture.listen =
        BindAddress(SocketAddr::new(config.aperture.listen.ip(), rpc_port));
    config.metrics.address =
        BindAddress(SocketAddr::new(config.metrics.address.ip(), metrics_port));

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
        start_magic_block_validator_with_config(
            &paths,
            "TEST",
            loaded_chain_accounts,
        ),
        rpc_port,
    )
}

pub fn start_magicblock_validator_with_config_struct_and_temp_dir(
    config: ValidatorParams,
    loaded_chain_accounts: &LoadedAccounts,
    default_tmpdir: TempDir,
    temp_dir: PathBuf,
) -> (TempDir, Option<process::Child>, u16) {
    let rpc_port = resolve_port(config.aperture.listen.ip(), &[]);
    let metrics_port = resolve_port(config.metrics.address.ip(), &[rpc_port]);

    let mut config = config.clone();
    config.aperture.listen =
        BindAddress(SocketAddr::new(config.aperture.listen.ip(), rpc_port));
    config.metrics.address =
        BindAddress(SocketAddr::new(config.metrics.address.ip(), metrics_port));

    let workspace_dir = resolve_workspace_dir();
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
        ),
        rpc_port,
    )
}

pub fn cleanup(validator: &mut Child) {
    let _ = validator.kill().inspect_err(|e| {
        eprintln!("ERR: Failed to kill validator: {:?}", e);
    });
    let _ = validator.wait().inspect_err(|e| {
        eprintln!("ERR: Failed to reap validator: {:?}", e);
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
    programs: Option<Vec<LoadableProgram>>,
) -> Vec<LoadableProgram> {
    programs
        .map(|programs| {
            programs
                .into_iter()
                .map(|program| LoadableProgram {
                    id: program.id,
                    path: path_relative_to_workspace(&format!(
                        "target/deploy/{}",
                        program.path.display()
                    ))
                    .into(),
                })
                .collect()
        })
        .unwrap_or_default()
}

// -----------------
// Utilities
// -----------------

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
