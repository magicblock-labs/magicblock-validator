use integration_test_tools::loaded_accounts::LoadedAccounts;
use integration_test_tools::validator::start_test_validator_with_config;
use integration_test_tools::{
    toml_to_args::ProgramLoader,
    validator::{
        resolve_workspace_dir, start_magic_block_validator_with_config,
        TestRunnerPaths,
    },
};
use std::{
    error::Error,
    io,
    path::Path,
    process::{self, Output},
};
use teepee::Teepee;
use test_runner::cleanup::{
    cleanup_devnet_only, cleanup_validator, cleanup_validators,
};

pub fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // let Ok((security_output, scenarios_output)) =
    //     run_schedule_commit_tests(&manifest_dir)
    // else {
    //     // If any test run panics (i.e. not just a failing test) then we bail
    //     return;
    // };
    // let Ok(issues_frequent_commits_output) =
    //     run_issues_frequent_commmits_tests(&manifest_dir)
    // else {
    //     return;
    // };
    // let Ok(cloning_output) = run_cloning_tests(&manifest_dir) else {
    //     return;
    // };
    //
    // let Ok(restore_ledger_output) = run_restore_ledger_tests(&manifest_dir)
    // else {
    //     return;
    // };
    //
    // let Ok(magicblock_api_output) = run_magicblock_api_tests(&manifest_dir)
    // else {
    //     return;
    // };
    //
    // let Ok((table_mania_output, committor_output)) =
    //     run_table_mania_and_committor_tests(&manifest_dir)
    // else {
    //     return;
    // };
    // let Ok(magicblock_pubsub_output) =
    //     run_magicblock_pubsub_tests(&manifest_dir)
    // else {
    //     return;
    // };

    let Ok(config_output) = run_config_tests(&manifest_dir) else {
        return;
    };

    // Assert that all tests passed
    // assert_cargo_tests_passed(security_output, "security");
    // assert_cargo_tests_passed(scenarios_output, "scenarios");
    // assert_cargo_tests_passed(cloning_output, "cloning");
    // assert_cargo_tests_passed(
    //     issues_frequent_commits_output,
    //     "issues_frequent_commits",
    // );
    // assert_cargo_tests_passed(restore_ledger_output, "restore_ledger");
    // assert_cargo_tests_passed(magicblock_api_output, "magicblock_api");
    // assert_cargo_tests_passed(table_mania_output, "table_mania");
    // assert_cargo_tests_passed(committor_output, "committor");
    // assert_cargo_tests_passed(magicblock_pubsub_output, "magicblock_pubsub");
    assert_cargo_tests_passed(config_output, "config");
}

fn should_run_test(test_name: &str) -> bool {
    let run = std::env::var("RUN_TESTS")
        .map(|tests| tests.split(',').any(|t| t.trim() == test_name))
        .unwrap_or(true);

    if !run {
        eprintln!("Skipping {test_name} since the RUN_TESTS env var does not include it");
        return false;
    }

    let skip = std::env::var("SKIP_TESTS")
        .map(|tests| tests.split(',').any(|t| t.trim() == test_name))
        .unwrap_or(false);

    if skip {
        eprintln!(
            "Skipping {test_name} since the SKIP_TESTS env var includes it"
        );
        false
    } else {
        true
    }
}

fn success_output() -> Output {
    Output {
        status: process::ExitStatus::default(),
        stdout: vec![],
        stderr: vec![],
    }
}

// -----------------
// Tests
// -----------------
fn run_restore_ledger_tests(
    manifest_dir: &str,
) -> Result<Output, Box<dyn Error>> {
    if !should_run_test("restore_ledger") {
        return Ok(success_output());
    }
    eprintln!("======== RUNNING RESTORE LEDGER TESTS ========");
    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();
    // The ledger tests manage their own ephem validator so all we start up here
    // is devnet
    let mut devnet_validator = match start_validator(
        "restore-ledger-conf.devnet.toml",
        ValidatorCluster::Chain(None),
        &loaded_chain_accounts,
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

fn run_table_mania_and_committor_tests(
    manifest_dir: &str,
) -> Result<(Output, Output), Box<dyn Error>> {
    eprintln!("======== Starting DEVNET Validator for TableMania and Committor ========");

    let (run_table_mania, run_committor) =
        (should_run_test("table_mania"), should_run_test("committor"));

    if !run_table_mania && !run_committor {
        eprintln!("Skipping table mania and committor tests");
        return Ok((success_output(), success_output()));
    }

    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();

    let mut devnet_validator = match start_validator(
        "committor-conf.devnet.toml",
        ValidatorCluster::Chain(None),
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };

    // NOTE: the table mania and committor tests run directly against
    // a chain validator therefore no ephemeral validator needs to be started

    let table_mania_test_output = if run_table_mania {
        let test_table_mania_dir =
            format!("{}/../{}", manifest_dir, "test-table-mania");

        match run_test(test_table_mania_dir, Default::default()) {
            Ok(output) => output,
            Err(err) => {
                eprintln!("Failed to run table-mania: {:?}", err);
                cleanup_devnet_only(&mut devnet_validator);
                return Err(err.into());
            }
        }
    } else {
        eprintln!("Skipping table mania tests");
        success_output()
    };

    let committor_test_output = if run_committor {
        let test_committor_dir = format!(
            "{}/../{}",
            manifest_dir, "schedulecommit/committor-service"
        );
        eprintln!("Running committor tests in {}", test_committor_dir);
        match run_test(test_committor_dir, Default::default()) {
            Ok(output) => output,
            Err(err) => {
                eprintln!("Failed to run committor: {:?}", err);
                cleanup_devnet_only(&mut devnet_validator);
                return Err(err.into());
            }
        }
    } else {
        eprintln!("Skipping committor tests");
        success_output()
    };

    cleanup_devnet_only(&mut devnet_validator);

    Ok((table_mania_test_output, committor_test_output))
}

fn run_schedule_commit_tests(
    manifest_dir: &str,
) -> Result<(Output, Output), Box<dyn Error>> {
    if !should_run_test("schedulecommit") {
        return Ok((success_output(), success_output()));
    }
    eprintln!(
        "======== Starting DEVNET Validator for Scenarios + Security ========"
    );

    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();

    // Start validators via `cargo run --release  -- <config>
    let mut devnet_validator = match start_validator(
        "schedulecommit-conf.devnet.toml",
        ValidatorCluster::Chain(None),
        &loaded_chain_accounts,
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
        "schedulecommit-conf-fees.ephem.toml",
        ValidatorCluster::Ephem,
        &loaded_chain_accounts,
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
    if !should_run_test("issues_frequent_commmits") {
        return Ok(success_output());
    }

    eprintln!("======== RUNNING ISSUES TESTS - Frequent Commits ========");
    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();
    let mut devnet_validator = match start_validator(
        "schedulecommit-conf.devnet.toml",
        ValidatorCluster::Chain(None),
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };
    let mut ephem_validator = match start_validator(
        "schedulecommit-conf.ephem.frequent-commits.toml",
        ValidatorCluster::Ephem,
        &loaded_chain_accounts,
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
    if !should_run_test("cloning") {
        return Ok(success_output());
    }
    eprintln!("======== RUNNING CLONING TESTS ========");
    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();

    let mut devnet_validator = match start_validator(
        "cloning-conf.devnet.toml",
        ValidatorCluster::Chain(Some(ProgramLoader::UpgradeableProgram)),
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };
    let mut ephem_validator = match start_validator(
        "cloning-conf.ephem.toml",
        ValidatorCluster::Ephem,
        &loaded_chain_accounts,
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

fn run_magicblock_api_tests(
    manifest_dir: &str,
) -> Result<Output, Box<dyn Error>> {
    if !should_run_test("magicblock_api") {
        return Ok(success_output());
    }

    eprintln!("======== RUNNING MAGICBLOCK API TESTS ========");

    let mut devnet_validator = match start_validator(
        "schedulecommit-conf.devnet.toml",
        ValidatorCluster::Chain(None),
        &LoadedAccounts::default(),
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };
    let mut ephem_validator = match start_validator(
        "validator-api-offline.devnet.toml",
        ValidatorCluster::Ephem,
        &LoadedAccounts::with_delegation_program_test_authority(),
    ) {
        Some(validator) => validator,
        None => {
            devnet_validator
                .kill()
                .expect("Failed to kill devnet validator");
            panic!("Failed to start ephemeral validator properly");
        }
    };

    let test_dir = format!("{}/../{}", manifest_dir, "test-magicblock-api");
    eprintln!("Running magicblock-api tests in {}", test_dir);

    let output = run_test(test_dir, Default::default()).map_err(|err| {
        eprintln!("Failed to magicblock api tests: {:?}", err);
        cleanup_validators(&mut ephem_validator, &mut devnet_validator);
        err
    })?;

    cleanup_validators(&mut ephem_validator, &mut devnet_validator);
    Ok(output)
}

fn run_magicblock_pubsub_tests(
    manifest_dir: &str,
) -> Result<Output, Box<dyn Error>> {
    if !should_run_test("pubsub") {
        return Ok(success_output());
    }
    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();

    let mut ephem_validator = match start_validator(
        "validator-offline.devnet.toml",
        ValidatorCluster::Ephem,
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start ephemeral validator properly");
        }
    };

    let test_dir = format!("{}/../{}", manifest_dir, "test-pubsub");
    eprintln!("Running magicblock pubsub tests in {}", test_dir);

    let output = run_test(test_dir, Default::default()).map_err(|err| {
        eprintln!("Failed to magicblock pubsub tests: {:?}", err);
        cleanup_validator(&mut ephem_validator, "ephemeral");
        err
    })?;

    cleanup_validator(&mut ephem_validator, "ephemeral");
    Ok(output)
}

fn run_config_tests(manifest_dir: &str) -> Result<Output, Box<dyn Error>> {
    if !should_run_test("config") {
        return Ok(success_output());
    }
    eprintln!("======== RUNNING CONFIG TESTS ========");
    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();

    // Initialize only a devnet cluster for config tests
    let mut devnet_validator = match start_validator(
        "config-conf.devnet.toml",
        ValidatorCluster::Chain(Some(ProgramLoader::UpgradeableProgram)),
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };

    let test_config_dir = format!("{}/../{}", manifest_dir, "test-config");
    eprintln!("Running config tests in {}", test_config_dir);
    let output = match run_test(test_config_dir, Default::default()) {
        Ok(output) => output,
        Err(err) => {
            eprintln!("Failed to run config tests: {:?}", err);
            cleanup_devnet_only(&mut devnet_validator);
            return Err(err.into());
        }
    };
    cleanup_devnet_only(&mut devnet_validator);
    Ok(output)
}

// -----------------
// Configs/Checks
// -----------------
fn assert_cargo_tests_passed(output: process::Output, test_name: &str) {
    if !output.status.success() {
        eprintln!("cargo test '{}'", test_name);
        eprintln!("status: {}", output.status);
        eprintln!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    } else if std::env::var("DUMP").is_ok() {
        eprintln!("cargo test success '{}'", test_name);
        eprintln!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    }
    // If a test in the suite fails the status shows that
    assert!(output.status.success(), "cargo test failed '{}'", test_name);
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
    loaded_chain_accounts: &LoadedAccounts,
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
                loaded_chain_accounts,
                log_suffix,
            )
        }
        _ => start_magic_block_validator_with_config(
            &test_runner_paths,
            log_suffix,
            loaded_chain_accounts,
            false,
        ),
    }
}
