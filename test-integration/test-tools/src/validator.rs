use std::{
    net::TcpStream,
    path::{Path, PathBuf},
    process::{self, Child},
    thread::sleep,
    time::Duration,
};

use crate::toml_to_args::{
    config_to_args, rpc_port_from_config, ProgramLoader,
};

pub fn start_magic_block_validator_with_config(
    test_runner_paths: &TestRunnerPaths,
    log_suffix: &str,
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
    command.arg("build");
    if release {
        command.arg("--release");
    }
    let build_res = command.current_dir(root_dir.clone()).output();

    if build_res.map_or(false, |output| !output.status.success()) {
        eprintln!("Failed to build validator");
        return None;
    }

    // Start validator via `cargo run -- <path to config>`
    let mut command = process::Command::new("cargo");
    command.arg("run");
    if release {
        command.arg("--release");
    }
    command
        .arg("--")
        .arg(config_path)
        .env("RUST_LOG_STYLE", log_suffix)
        .current_dir(root_dir);

    eprintln!("Starting validator with {:?}", command);

    let validator = command.spawn().expect("Failed to start validator");
    wait_for_validator(validator, port)
}

pub fn start_test_validator_with_config(
    test_runner_paths: &TestRunnerPaths,
    program_loader: Option<ProgramLoader>,
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
            "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev",
            "validator-authority.json",
        ),
        (
            "LUzidNSiPNjYNkxZcUm5hYHwnWPwsUfh2US1cpWwaBm",
            "luzid-authority.json",
        ),
        (
            "EpJnX7ueXk7fKojBymqmVuCuwyhDQsYcLVL1XMsBbvDX",
            "validator-fees-vault.json",
        ),
        (
            "7JrkjmZPprHwtuvtuGTXp9hwfGYFAQLnLeFM52kqAgXg",
            "protocol-fees-vault.json",
        ),
        (
            "9yXjZTevvMp1XgZSZEaziPRgFiXtAQChpnP2oX9eCpvt",
            "non-delegated-cloneable-account1.json",
        ),
        (
            "BHBuATGifAD4JbRpM5nVdyhKzPgv3p2CxLEHAqwBzAj5",
            "non-delegated-cloneable-account2.json",
        ),
        (
            "2o48ieM95rmHqMWC5B3tTX4DL7cLm4m1Kuwjay3keQSv",
            "non-delegated-cloneable-account3.json",
        ),
        (
            "2EmfL3MqL3YHABudGNmajjCpR13NNEn9Y4LWxbDm6SwR",
            "non-delegated-cloneable-account4.json",
        ),
    ];

    let account_args = accounts
        .iter()
        .flat_map(|(account, file)| {
            let account_path = accounts_dir.join(file).canonicalize().unwrap();
            vec![
                "--account".to_string(),
                account.to_string(),
                account_path.to_str().unwrap().to_string(),
            ]
        })
        .collect::<Vec<_>>();

    args.extend(account_args);

    let mut command = process::Command::new("solana-test-validator");
    command
        .args(args)
        .env("RUST_LOG", "solana=warn")
        .env("RUST_LOG_STYLE", log_suffix)
        .current_dir(root_dir);

    eprintln!("Starting test validator with {:?}", command);
    let validator = command.spawn().expect("Failed to start validator");
    wait_for_validator(validator, port)
}

pub fn wait_for_validator(mut validator: Child, port: u16) -> Option<Child> {
    const SLEEP_DURATION: Duration = Duration::from_millis(400);
    let max_retries = if std::env::var("CI").is_ok() {
        1500
    } else {
        75
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
