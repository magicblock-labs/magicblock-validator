use integration_test_tools::{
    toml_to_args::{config_to_args, rpc_port_from_config, ProgramLoader},
    validator::{
        resolve_workspace_dir, start_magic_block_validator_with_config,
        wait_for_validator, TestRunnerPaths,
    },
};
use std::{
    error::Error,
    io,
    path::Path,
    process::{self, Output},
};
use teepee::Teepee;
use test_runner::cleanup::{cleanup_devnet_only, cleanup_validators};

pub fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    let Ok((security_output, scenarios_output)) =
        run_schedule_commit_tests(&manifest_dir)
    else {
        return;
    };

    let Ok(issues_frequent_commits_output) =
        run_issues_frequent_commmits_tests(&manifest_dir)
    else {
        return;
    };
    let Ok(cloning_output) = run_cloning_tests(&manifest_dir) else {
        return;
    };

    let Ok(restore_ledger_output) = run_restore_ledger_tests(&manifest_dir)
    else {
        return;
    };

    // Assert that all tests passed
    assert_cargo_tests_passed(restore_ledger_output);
    assert_cargo_tests_passed(security_output);
    assert_cargo_tests_passed(scenarios_output);
    assert_cargo_tests_passed(cloning_output);
    assert_cargo_tests_passed(issues_frequent_commits_output);
}

// -----------------
// Tests
// -----------------
fn run_restore_ledger_tests(
    manifest_dir: &str,
) -> Result<Output, Box<dyn Error>> {
    eprintln!("======== RUNNING RESTORE LEDGER TESTS ========");
    // The ledger tests manage their own ephem validator so all we start up here
    // is devnet
    let mut devnet_validator = match start_validator(
        "restore-ledger-conf.devnet.toml",
        ValidatorCluster::Chain(None),
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };
    let test_restore_ledger_dir =
        format!("{}/../{}", manifest_dir, "test-ledger-restore");
    eprintln!(
        "Running restore ledger tests in {}",
        test_restore_ledger_dir
    );
    let output = match run_test(test_restore_ledger_dir, Default::default()) {
        Ok(output) => output,
        Err(err) => {
            eprintln!("Failed to run restore ledger tests: {:?}", err);
            cleanup_devnet_only(&mut devnet_validator);
            return Err(err.into());
        }
    };
    cleanup_devnet_only(&mut devnet_validator);
    Ok(output)
}

fn run_schedule_commit_tests(
    manifest_dir: &str,
) -> Result<(Output, Output), Box<dyn Error>> {
    eprintln!(
        "======== Starting DEVNET Validator for Scenarios + Security ========"
    );

    // Start validators via `cargo run --release  -- <config>
    let mut devnet_validator = match start_validator(
        "schedulecommit-conf.devnet.toml",
        ValidatorCluster::Chain(None),
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };

    // These share a common config that includes the program to schedule commits
    // Thus they can run against the same validator instances
    eprintln!(
        "======== Starting EPHEM Validator for Scenarios + Security ========"
    );
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
    let test_security_dir =
        format!("{}/../{}", manifest_dir, "schedulecommit/test-security");
    eprintln!("Running security tests in {}", test_security_dir);
    let test_security_output =
        match run_test(test_security_dir, Default::default()) {
            Ok(output) => output,
            Err(err) => {
                eprintln!("Failed to run security: {:?}", err);
                cleanup_validators(&mut ephem_validator, &mut devnet_validator);
                return Err(err.into());
            }
        };

    eprintln!("======== RUNNING SCENARIOS TESTS ========");
    let test_scenarios_dir =
        format!("{}/../{}", manifest_dir, "schedulecommit/test-scenarios");
    let test_scenarios_output =
        match run_test(test_scenarios_dir, Default::default()) {
            Ok(output) => output,
            Err(err) => {
                eprintln!("Failed to run scenarios: {:?}", err);
                cleanup_validators(&mut ephem_validator, &mut devnet_validator);
                return Err(err.into());
            }
        };

    cleanup_validators(&mut ephem_validator, &mut devnet_validator);
    Ok((test_security_output, test_scenarios_output))
}

fn run_issues_frequent_commmits_tests(
    manifest_dir: &str,
) -> Result<Output, Box<dyn Error>> {
    eprintln!("======== RUNNING ISSUES TESTS - Frequent Commits ========");
    let mut devnet_validator = match start_validator(
        "schedulecommit-conf.devnet.toml",
        ValidatorCluster::Chain(None),
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
    let test_issues_dir = format!("{}/../{}", manifest_dir, "test-issues");
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
            cleanup_validators(&mut ephem_validator, &mut devnet_validator);
            return Err(err.into());
        }
    };
    cleanup_validators(&mut ephem_validator, &mut devnet_validator);
    Ok(test_output)
}

fn run_cloning_tests(manifest_dir: &str) -> Result<Output, Box<dyn Error>> {
    eprintln!("======== RUNNING CLONING TESTS ========");
    let mut devnet_validator = match start_validator(
        "cloning-conf.devnet.toml",
        ValidatorCluster::Chain(Some(ProgramLoader::BpfProgram)),
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };
    let mut ephem_validator = match start_validator(
        "cloning-conf.ephem.toml",
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
    let test_cloning_dir = format!("{}/../{}", manifest_dir, "test-cloning");
    eprintln!("Running cloning tests in {}", test_cloning_dir);
    let output = match run_test(test_cloning_dir, Default::default()) {
        Ok(output) => output,
        Err(err) => {
            eprintln!("Failed to run cloning tests: {:?}", err);
            cleanup_validators(&mut ephem_validator, &mut devnet_validator);
            return Err(err.into());
        }
    };
    cleanup_validators(&mut ephem_validator, &mut devnet_validator);
    Ok(output)
}

// -----------------
// Configs/Checks
// -----------------
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
    cmd.current_dir(manifest_dir.clone());
    Teepee::new(cmd).output()
}

// -----------------
// Validator Startup
// -----------------
fn resolve_paths(config_file: &str) -> TestRunnerPaths {
    let workspace_dir = resolve_workspace_dir();
    let root_dir = Path::new(&workspace_dir)
        .join("..")
        .canonicalize()
        .unwrap()
        .to_path_buf();
    let config_path =
        Path::new(&workspace_dir).join("configs").join(config_file);
    TestRunnerPaths {
        config_path,
        root_dir,
        workspace_dir,
    }
}

enum ValidatorCluster {
    Chain(Option<ProgramLoader>),
    Ephem,
}

impl ValidatorCluster {
    fn log_suffix(&self) -> &'static str {
        match self {
            ValidatorCluster::Chain(_) => "CHAIN",
            ValidatorCluster::Ephem => "EPHEM",
        }
    }
}

fn start_validator(
    config_file: &str,
    cluster: ValidatorCluster,
) -> Option<process::Child> {
    let log_suffix = cluster.log_suffix();
    let test_runner_paths = resolve_paths(config_file);

    match cluster {
        ValidatorCluster::Chain(program_loader)
            if std::env::var("FORCE_MAGIC_BLOCK_VALIDATOR").is_err() =>
        {
            start_test_validator_with_config(
                &test_runner_paths,
                program_loader,
                log_suffix,
            )
        }
        _ => start_magic_block_validator_with_config(
            &test_runner_paths,
            log_suffix,
            false,
        ),
    }
}

fn start_test_validator_with_config(
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
