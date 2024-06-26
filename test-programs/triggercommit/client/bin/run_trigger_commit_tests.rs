use std::{path::Path, process, thread::sleep, time::Duration};

pub fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    // Start validator via `cargo run --release  -- test-programs/triggercommit/triggercommit-conf.toml
    let mut validator = start_validator_with_config(
        "test-programs/triggercommit/triggercommit-conf.toml",
    );

    // Wait for validator to come up (TODO: improve by detecting RPC is listening)
    sleep(Duration::from_secs(2));

    // Run cargo run --bin trigger-commit-direct
    let trigger_commit_direct_output =
        run_bin(manifest_dir.clone(), "trigger-commit-direct");

    // Run cargo run --bin trigger-commit-direct
    let trigger_commit_cpi_output =
        run_bin(manifest_dir.clone(), "trigger-commit-cpi-ix");

    // Kill Validator
    validator.kill().expect("Failed to kill validator");

    // Assert that the test passed
    assert_output(trigger_commit_direct_output, "trigger-commit-direct");
    assert_output(trigger_commit_cpi_output, "trigger-commit-cpi-ix");
}

fn assert_output(output: process::Output, test_name: &str) {
    if !output.status.success() {
        eprintln!("{} non-success status", test_name);
        eprintln!("status: {}", output.status);
        eprintln!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    }
    assert!(
        output.status.success(),
        "{} status success failed",
        test_name
    );
    assert!(String::from_utf8_lossy(&output.stdout).ends_with("Success\n"));
}

fn run_bin(manifest_dir: String, bin_name: &str) -> process::Output {
    process::Command::new("cargo")
        .arg("run")
        .arg("--bin")
        .arg(bin_name)
        .current_dir(manifest_dir.clone())
        .output()
        .unwrap_or_else(|_| panic!("Failed to start '{}'", bin_name))
}

fn start_validator_with_config(config_path: &str) -> process::Child {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace_dir = Path::new(&manifest_dir).join("..").join("..");
    let root_dir = Path::new(&workspace_dir).join("..");

    // Start validator via `cargo run --release  -- test-programs/triggercommit/triggercommit-conf.toml
    process::Command::new("cargo")
        .arg("run")
        .arg("--release")
        .arg("--")
        .arg(config_path)
        .current_dir(root_dir)
        .spawn()
        .expect("Failed to start validator")
}
