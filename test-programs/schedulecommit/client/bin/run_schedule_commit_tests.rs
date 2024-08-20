use std::{
    io,
    net::TcpStream,
    path::Path,
    process::{self, Child},
    thread::sleep,
    time::Duration,
};

fn cleanup(ephem_validator: &mut Child, devnet_validator: &mut Child) {
    ephem_validator
        .kill()
        .expect("Failed to kill ephemeral validator");
    devnet_validator
        .kill()
        .expect("Failed to kill devnet validator");
}

pub fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    // Start validators via `cargo run --release  -- <config>
    let mut devnet_validator = match start_validator_with_config(
        "test-programs/schedulecommit/configs/schedulecommit-conf.devnet.toml",
        7799,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };
    let mut ephem_validator = match start_validator_with_config(
        "test-programs/schedulecommit/configs/schedulecommit-conf.ephem.toml",
        8899,
    ) {
        Some(validator) => validator,
        None => {
            devnet_validator
                .kill()
                .expect("Failed to kill devnet validator");
            panic!("Failed to start ephemeral validator properly");
        }
    };

    let test_security_dir =
        format!("{}/../{}", manifest_dir.clone(), "test-security");
    let test_security_output = match run_test(test_security_dir) {
        Ok(output) => output,
        Err(err) => {
            eprintln!("Failed to run security: {:?}", err);
            cleanup(&mut ephem_validator, &mut devnet_validator);
            return;
        }
    };
    let test_scenarios_dir =
        format!("{}/../{}", manifest_dir.clone(), "test-scenarios");
    let test_scenarios_output = match run_test(test_scenarios_dir) {
        Ok(output) => output,
        Err(err) => {
            eprintln!("Failed to run scenarios: {:?}", err);
            cleanup(&mut ephem_validator, &mut devnet_validator);
            return;
        }
    };

    // Kill Validators
    cleanup(&mut ephem_validator, &mut devnet_validator);

    // Assert that both test suites passed
    assert_cargo_tests_passed(test_security_output);
    assert_cargo_tests_passed(test_scenarios_output);
}

fn assert_cargo_tests_passed(output: process::Output) {
    if !output.status.success() {
        eprintln!("cargo test");
        eprintln!("status: {}", output.status);
        eprintln!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    } else if std::env::var("DUMP").is_ok() {
        eprintln!("cargo test success");
        eprintln!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    }
    // If a test in the suite fails the status shows that
    assert!(output.status.success(), "cargo test failed");
}

fn run_test(manifest_dir: String) -> io::Result<process::Output> {
    process::Command::new("cargo")
        .arg("test")
        .arg("--")
        .arg("--nocapture")
        .current_dir(manifest_dir.clone())
        .output()
}

fn start_validator_with_config(
    config_path: &str,
    port: u16,
) -> Option<process::Child> {
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

    // Wait until the validator is listening on 0.0.0.0:<port>
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
