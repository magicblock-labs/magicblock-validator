use std::{net::TcpStream, path::Path, process, thread::sleep, time::Duration};

pub fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    // Start validator via `cargo run --release  -- test-programs/triggercommit/triggercommit-conf.toml
    let mut validator = match start_validator_with_config(
        "test-programs/triggercommit/triggercommit-conf.toml",
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start validator properly");
        }
    };

    // Run cargo run --bin trigger-commit-direct
    let trigger_commit_direct_output =
        match run_bin(manifest_dir.clone(), "trigger-commit-direct") {
            Ok(output) => output,
            Err(err) => {
                eprintln!("Failed to run trigger-commit-direct: {:?}", err);
                validator.kill().expect("Failed to kill validator");
                return;
            }
        };

    // Run cargo run --bin trigger-commit-cpi-ix
    let trigger_commit_cpi_output =
        match run_bin(manifest_dir.clone(), "trigger-commit-cpi-ix") {
            Ok(output) => output,
            Err(err) => {
                eprintln!("Failed to run trigger-commit-cpi: {:?}", err);
                validator.kill().expect("Failed to kill validator");
                return;
            }
        };

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
fn run_bin(
    manifest_dir: String,
    bin_name: &str,
) -> io::Result<process::Output> {
    process::Command::new("cargo")
        .arg("run")
        .arg("--bin")
        .arg(bin_name)
        .current_dir(manifest_dir.clone())
        .output()
}

fn start_validator_with_config(config_path: &str) -> Option<process::Child> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace_dir = Path::new(&manifest_dir).join("..").join("..");
    let root_dir = Path::new(&workspace_dir).join("..");

    // First build so that the validator can start fast
    let build_res = process::Command::new("cargo")
        .arg("build")
        .current_dir(root_dir.clone())
        .output();

    if build_res.map_or(false, |output| !output.status.success()) {
        eprintln!("Failed to build validator");
        return None;
    }

    // Start validator via `cargo run -- test-programs/triggercommit/triggercommit-conf.toml
    let mut validator = process::Command::new("cargo")
        .arg("run")
        .arg("--")
        .arg(config_path)
        .current_dir(root_dir)
        .spawn()
        .expect("Failed to start validator");

    // Wait until the validator is listening on 0.0.0.0:8899
    let mut count = 0;
    loop {
        if TcpStream::connect("0.0.0.0:8899").is_ok() {
            break Some(validator);
        }
        count += 1;
        if count >= 5 * 5 {
            eprintln!("Validator RPC failed to listen");
            validator.kill().expect("Failed to kill validator");
            return None;
        }
        sleep(Duration::from_millis(400));
    }
}
