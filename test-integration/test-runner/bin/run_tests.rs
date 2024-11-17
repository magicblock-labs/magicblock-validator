use integration_test_tools::toml_to_args::{
    config_to_args, rpc_port_from_config,
};
use std::{
    io,
    net::TcpStream,
    path::{Path, PathBuf},
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
    // -----------------
    // Commons Scenarios and Security Tests
    // -----------------
    // These share a common config that includes the program to schedule commits
    // Thus they can run against the same validator instances
    let (security_output, scenarios_output) = {
        eprintln!("======== Starting DEVNET Validator for Scenarios + Security ========");

        // Start validators via `cargo run --release  -- <config>
        let mut devnet_validator = match start_validator(
            "schedulecommit-conf.devnet.toml",
            ValidatorCluster::Chain,
        ) {
            Some(validator) => validator,
            None => {
                panic!("Failed to start devnet validator properly");
            }
        };

        eprintln!("======== Starting EPHEM Validator for Scenarios + Security ========");
        let mut ephem_validator = match start_validator(
            "schedulecommit-conf.ephem.toml",
            ValidatorCluster::Ephem,
        ) {
            Some(validator) => validator,
            None => {
                devnet_validator
                    .kill()
                    .expect("Failed to kill devnet validator");
                panic!("Failed to start ephemeral validator properly");
            }
        };

        eprintln!("======== RUNNING SECURITY TESTS ========");
        let test_security_dir = format!(
            "{}/../{}",
            manifest_dir.clone(),
            "schedulecommit/test-security"
        );
        eprintln!("Running security tests in {}", test_security_dir);
        let test_security_output =
            match run_test(test_security_dir, Default::default()) {
                Ok(output) => output,
                Err(err) => {
                    eprintln!("Failed to run security: {:?}", err);
                    cleanup(&mut ephem_validator, &mut devnet_validator);
                    return;
                }
            };

        eprintln!("======== RUNNING SCENARIOS TESTS ========");
        let test_scenarios_dir = format!(
            "{}/../{}",
            manifest_dir.clone(),
            "schedulecommit/test-scenarios"
        );
        let test_scenarios_output =
            match run_test(test_scenarios_dir, Default::default()) {
                Ok(output) => output,
                Err(err) => {
                    eprintln!("Failed to run scenarios: {:?}", err);
                    cleanup(&mut ephem_validator, &mut devnet_validator);
                    return;
                }
            };

        cleanup(&mut ephem_validator, &mut devnet_validator);
        (test_security_output, test_scenarios_output)
    };

    // The following tests need custom validator setups.
    // Therefore, we start the validators again with custom configs for those tests.

    // -----------------
    // Issues: Frequent Commits
    // -----------------
    let issues_frequent_commits_output = {
        eprintln!("======== RUNNING ISSUES TESTS - Frequent Commits ========");
        let mut devnet_validator = match start_validator(
            "schedulecommit-conf.devnet.toml",
            ValidatorCluster::Chain,
        ) {
            Some(validator) => validator,
            None => {
                panic!("Failed to start devnet validator properly");
            }
        };
        let mut ephem_validator = match start_validator(
            "schedulecommit-conf.ephem.frequent-commits.toml",
            ValidatorCluster::Ephem,
        ) {
            Some(validator) => validator,
            None => {
                devnet_validator
                    .kill()
                    .expect("Failed to kill devnet validator");
                panic!("Failed to start ephemeral validator properly");
            }
        };
        let test_issues_dir =
            format!("{}/../{}", manifest_dir.clone(), "test-issues");
        let test_output = match run_test(
            test_issues_dir,
            RunTestConfig {
                package: Some("test-issues"),
                test: Some("test_frequent_commits_do_not_run_when_no_accounts_need_to_be_committed"),
            },
        ) {
            Ok(output) => output,
            Err(err) => {
                eprintln!("Failed to run issues: {:?}", err);
                cleanup(&mut ephem_validator, &mut devnet_validator);
                return;
            }
        };
        cleanup(&mut ephem_validator, &mut devnet_validator);
        test_output
    };

    // Assert that all tests passed
    assert_cargo_tests_passed(security_output);
    assert_cargo_tests_passed(scenarios_output);
    assert_cargo_tests_passed(issues_frequent_commits_output);
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

#[derive(Default)]
struct RunTestConfig<'a> {
    package: Option<&'a str>,
    test: Option<&'a str>,
}

fn run_test(
    manifest_dir: String,
    config: RunTestConfig,
) -> io::Result<process::Output> {
    let mut cmd = process::Command::new("cargo");
    cmd.env(
        "RUST_LOG",
        std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
    )
    .arg("test");
    if let Some(package) = config.package {
        cmd.arg("-p").arg(package);
    }
    if let Some(test) = config.test {
        cmd.arg(format!("'{}'", test));
    }
    cmd.arg("--").arg("--test-threads=1").arg("--nocapture");
    cmd.current_dir(manifest_dir.clone()).output()
}

// -----------------
// Validator Startup
// -----------------
struct TestRunnerPaths {
    config_path: PathBuf,
    root_dir: PathBuf,
    workspace_dir: PathBuf,
}

fn resolve_paths(config_file: &str) -> TestRunnerPaths {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace_dir = Path::new(&manifest_dir)
        .join("..")
        .canonicalize()
        .unwrap()
        .to_path_buf();
    let root_dir = Path::new(&workspace_dir)
        .join("..")
        .canonicalize()
        .unwrap()
        .to_path_buf();
    let config_path = Path::new(&manifest_dir)
        .join("..")
        .join("configs")
        .join(config_file);
    TestRunnerPaths {
        config_path,
        root_dir,
        workspace_dir,
    }
}

fn wait_for_validator(mut validator: Child, port: u16) -> Option<Child> {
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

enum ValidatorCluster {
    Chain,
    Ephem,
}

impl ValidatorCluster {
    fn log_suffix(&self) -> &'static str {
        match self {
            ValidatorCluster::Chain => "CHAIN",
            ValidatorCluster::Ephem => "EPHEM",
        }
    }
}

fn start_validator(
    config_file: &str,
    cluster: ValidatorCluster,
) -> Option<process::Child> {
    let log_suffix = cluster.log_suffix();
    match cluster {
        ValidatorCluster::Chain
            if std::env::var("FORCE_MAGIC_BLOCK_VALIDATOR").is_err() =>
        {
            start_test_validator_with_config(config_file, log_suffix)
        }
        _ => start_magic_block_validator_with_config(config_file, log_suffix),
    }
}

fn start_magic_block_validator_with_config(
    config_file: &str,
    log_suffix: &str,
) -> Option<process::Child> {
    let TestRunnerPaths {
        config_path,
        root_dir,
        ..
    } = resolve_paths(config_file);
    let port = rpc_port_from_config(&config_path);

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

fn start_test_validator_with_config(
    config_file: &str,
    log_suffix: &str,
) -> Option<process::Child> {
    let TestRunnerPaths {
        config_path,
        root_dir,
        workspace_dir,
    } = resolve_paths(config_file);

    let port = rpc_port_from_config(&config_path);
    let mut args = config_to_args(&config_path);

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
        .env("RUST_LOG_STYLE", log_suffix)
        .current_dir(root_dir);

    eprintln!("Starting test validator with {:?}", command);
    let validator = command.spawn().expect("Failed to start validator");
    wait_for_validator(validator, port)
}
