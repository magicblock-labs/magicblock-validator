use crate::toml_to_args::rpc_port_from_config;
use std::{
    net::TcpStream,
    path::{Path, PathBuf},
    process::{self, Child},
    thread::sleep,
    time::Duration,
};

pub fn start_magic_block_validator_with_config(
    test_runner_paths: &TestRunnerPaths,
    log_suffix: &str,
) -> Option<process::Child> {
    let TestRunnerPaths {
        config_path,
        root_dir,
        ..
    } = test_runner_paths;

    let port = rpc_port_from_config(config_path);

    // First build so that the validator can start fast
    let build_res = process::Command::new("cargo")
        .arg("build")
        .current_dir(root_dir.clone())
        .output();

    if build_res.map_or(false, |output| !output.status.success()) {
        eprintln!("Failed to build validator");
        return None;
    }

    // Start validator via `cargo run -- <path to config>`
    let mut command = process::Command::new("cargo");
    command
        .arg("run")
        .arg("--")
        .arg(config_path)
        .env("RUST_LOG_STYLE", log_suffix)
        .current_dir(root_dir);

    eprintln!("Starting validator with {:?}", command);

    let validator = command.spawn().expect("Failed to start validator");
    wait_for_validator(validator, port)
}

pub fn wait_for_validator(mut validator: Child, port: u16) -> Option<Child> {
    let mut count = 0;
    loop {
        if TcpStream::connect(format!("0.0.0.0:{}", port)).is_ok() {
            break Some(validator);
        }
        count += 1;
        // 30 seconds
        if count >= 75 {
            eprintln!("Validator RPC on port {} failed to listen", port);
            validator.kill().expect("Failed to kill validator");
            break None;
        }
        sleep(Duration::from_millis(400));
    }
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
