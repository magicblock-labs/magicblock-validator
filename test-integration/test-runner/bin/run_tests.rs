use std::{
    error::Error,
    io,
    path::Path,
    process::{self, Output},
};

use integration_test_tools::{
    loaded_accounts::LoadedAccounts,
    toml_to_args::ProgramLoader,
    validator::{
        resolve_workspace_dir, start_magic_block_validator_with_config,
        start_test_validator_with_config, TestRunnerPaths,
    },
};
use teepee::Teepee;
use test_runner::{
    cleanup::{cleanup_devnet_only, cleanup_validator, cleanup_validators},
    env_config::TestConfigViaEnvVars,
    signal::wait_for_ctrlc,
};

pub fn main() {
    let config = TestConfigViaEnvVars::default();
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let Ok((security_output, scenarios_output)) =
        run_schedule_commit_tests(&manifest_dir, &config)
    else {
        // If any test run panics (i.e. not just a failing test) then we bail
        return;
    };

    let Ok(issues_frequent_commits_output) =
        run_issues_frequent_commmits_tests(&manifest_dir, &config)
    else {
        return;
    };

    let Ok(cloning_output) = run_cloning_tests(&manifest_dir, &config) else {
        return;
    };

    let Ok(restore_ledger_output) =
        run_restore_ledger_tests(&manifest_dir, &config)
    else {
        return;
    };

    let Ok(magicblock_api_output) =
        run_magicblock_api_tests(&manifest_dir, &config)
    else {
        return;
    };

    let Ok((table_mania_output, committor_output)) =
        run_table_mania_and_committor_tests(&manifest_dir, &config)
    else {
        return;
    };

    let Ok(magicblock_pubsub_output) =
        run_magicblock_pubsub_tests(&manifest_dir, &config)
    else {
        return;
    };

    let Ok(config_output) = run_config_tests(&manifest_dir, &config) else {
        return;
    };

    let Ok(schedule_intents_output) =
        run_schedule_intents_tests(&manifest_dir, &config)
    else {
        return;
    };

    let Ok(task_scheduler_output) = run_task_scheduler_tests(&manifest_dir)
    else {
        return;
    };

    // Assert that all tests passed
    assert_cargo_tests_passed(security_output, "security");
    assert_cargo_tests_passed(scenarios_output, "scenarios");
    assert_cargo_tests_passed(cloning_output, "cloning");
    assert_cargo_tests_passed(
        issues_frequent_commits_output,
        "issues_frequent_commits",
    );
    assert_cargo_tests_passed(restore_ledger_output, "restore_ledger");
    assert_cargo_tests_passed(magicblock_api_output, "magicblock_api");
    assert_cargo_tests_passed(table_mania_output, "table_mania");
    assert_cargo_tests_passed(committor_output, "committor");
    assert_cargo_tests_passed(magicblock_pubsub_output, "magicblock_pubsub");
    assert_cargo_tests_passed(config_output, "config");
    assert_cargo_tests_passed(schedule_intents_output, "schedule_intents");
    assert_cargo_tests_passed(task_scheduler_output, "task_scheduler");
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
    config: &TestConfigViaEnvVars,
) -> Result<Output, Box<dyn Error>> {
    const TEST_NAME: &str = "restore_ledger";
    if config.skip_entirely(TEST_NAME) {
        return Ok(success_output());
    }

    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();

    let start_devnet_validator = || match start_validator(
        "restore-ledger-conf.devnet.toml",
        ValidatorCluster::Chain(None),
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };

    if config.run_test(TEST_NAME) {
        eprintln!("======== RUNNING RESTORE LEDGER TESTS ========");

        let mut devnet_validator = start_devnet_validator();

        let test_restore_ledger_dir =
            format!("{}/../{}", manifest_dir, "test-ledger-restore");
        eprintln!(
            "Running restore ledger tests in {}",
            test_restore_ledger_dir
        );
        let output = match run_test(test_restore_ledger_dir, Default::default())
        {
            Ok(output) => output,
            Err(err) => {
                eprintln!("Failed to run restore ledger tests: {:?}", err);
                cleanup_devnet_only(&mut devnet_validator);
                return Err(err.into());
            }
        };
        cleanup_devnet_only(&mut devnet_validator);
        Ok(output)
    } else {
        let devnet_validator =
            config.setup_devnet(TEST_NAME).then(start_devnet_validator);
        wait_for_ctrlc(devnet_validator, None, success_output())
    }
}

fn run_table_mania_and_committor_tests(
    manifest_dir: &str,
    config: &TestConfigViaEnvVars,
) -> Result<(Output, Output), Box<dyn Error>> {
    const TABLE_MANIA_TEST: &str = "table_mania";
    const COMMITTOR_TEST: &str = "committor";

    // Continue if either test is not skipped entirely
    if config.skip_entirely(TABLE_MANIA_TEST)
        && config.skip_entirely(COMMITTOR_TEST)
    {
        eprintln!("Skipping table mania and committor tests");
        return Ok((success_output(), success_output()));
    }

    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();

    let start_devnet_validator = || match start_validator(
        "committor-conf.devnet.toml",
        ValidatorCluster::Chain(None),
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };

    // Check if we should run tests or just setup
    let run_table_mania = config.run_test(TABLE_MANIA_TEST);
    let run_committor = config.run_test(COMMITTOR_TEST);

    if run_table_mania || run_committor {
        eprintln!("======== Starting DEVNET Validator for TableMania and Committor ========");

        let mut devnet_validator = start_devnet_validator();

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
            let test_committor_dir =
                format!("{}/../{}", manifest_dir, "test-committor-service");
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
    } else {
        let setup_needed = config.setup_devnet(TABLE_MANIA_TEST)
            || config.setup_devnet(COMMITTOR_TEST);
        let devnet_validator = setup_needed.then(start_devnet_validator);
        Ok((
            wait_for_ctrlc(devnet_validator, None, success_output())?,
            success_output(),
        ))
    }
}

fn run_schedule_commit_tests(
    manifest_dir: &str,
    config: &TestConfigViaEnvVars,
) -> Result<(Output, Output), Box<dyn Error>> {
    const TEST_NAME: &str = "schedulecommit";
    if config.skip_entirely(TEST_NAME) {
        return Ok((success_output(), success_output()));
    }

    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();

    let start_devnet_validator = || match start_validator(
        "schedulecommit-conf.devnet.toml",
        ValidatorCluster::Chain(None),
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };

    let start_ephem_validator = || match start_validator(
        "schedulecommit-conf-fees.ephem.toml",
        ValidatorCluster::Ephem,
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start ephemeral validator properly");
        }
    };

    if config.run_test(TEST_NAME) {
        eprintln!(
            "======== Starting DEVNET Validator for Scenarios + Security ========"
        );

        let mut devnet_validator = start_devnet_validator();

        // These share a common config that includes the program to schedule commits
        // Thus they can run against the same validator instances
        eprintln!(
            "======== Starting EPHEM Validator for Scenarios + Security ========"
        );
        let mut ephem_validator = start_ephem_validator();

        eprintln!("======== RUNNING SECURITY TESTS ========");
        let test_security_dir =
            format!("{}/../{}", manifest_dir, "schedulecommit/test-security");
        eprintln!("Running security tests in {}", test_security_dir);
        let test_security_output =
            match run_test(test_security_dir, Default::default()) {
                Ok(output) => output,
                Err(err) => {
                    eprintln!("Failed to run security: {:?}", err);
                    cleanup_validators(
                        &mut ephem_validator,
                        &mut devnet_validator,
                    );
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
                    cleanup_validators(
                        &mut ephem_validator,
                        &mut devnet_validator,
                    );
                    return Err(err.into());
                }
            };

        cleanup_validators(&mut ephem_validator, &mut devnet_validator);
        Ok((test_security_output, test_scenarios_output))
    } else {
        let devnet_validator =
            config.setup_devnet(TEST_NAME).then(start_devnet_validator);
        let ephem_validator =
            config.setup_ephem(TEST_NAME).then(start_ephem_validator);
        eprintln!("Setup validator(s)");
        wait_for_ctrlc(devnet_validator, ephem_validator, success_output())?;
        Ok((success_output(), success_output()))
    }
}

fn run_issues_frequent_commmits_tests(
    manifest_dir: &str,
    config: &TestConfigViaEnvVars,
) -> Result<Output, Box<dyn Error>> {
    const TEST_NAME: &str = "issues_frequent_commmits";
    if config.skip_entirely(TEST_NAME) {
        return Ok(success_output());
    }

    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();

    let start_devnet_validator = || match start_validator(
        "schedulecommit-conf.devnet.toml",
        ValidatorCluster::Chain(None),
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };

    let start_ephem_validator = || match start_validator(
        "schedulecommit-conf.ephem.frequent-commits.toml",
        ValidatorCluster::Ephem,
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start ephemeral validator properly");
        }
    };

    if config.run_test(TEST_NAME) {
        eprintln!("======== RUNNING ISSUES TESTS - Frequent Commits ========");

        let mut devnet_validator = start_devnet_validator();
        let mut ephem_validator = start_ephem_validator();

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
    } else {
        let devnet_validator =
            config.setup_devnet(TEST_NAME).then(start_devnet_validator);
        let ephem_validator =
            config.setup_ephem(TEST_NAME).then(start_ephem_validator);
        eprintln!("Setup validator(s)");
        wait_for_ctrlc(devnet_validator, ephem_validator, success_output())
    }
}

fn run_cloning_tests(
    manifest_dir: &str,
    config: &TestConfigViaEnvVars,
) -> Result<Output, Box<dyn Error>> {
    const TEST_NAME: &str = "cloning";
    if config.skip_entirely(TEST_NAME) {
        return Ok(success_output());
    }

    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();

    let start_devnet_validator = || match start_validator(
        "cloning-conf.devnet.toml",
        ValidatorCluster::Chain(Some(ProgramLoader::UpgradeableProgram)),
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };

    let start_ephem_validator = || match start_validator(
        "cloning-conf.ephem.toml",
        ValidatorCluster::Ephem,
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start ephemeral validator properly");
        }
    };

    if config.run_test(TEST_NAME) {
        eprintln!("======== RUNNING CLONING TESTS ========");

        let mut devnet_validator = start_devnet_validator();
        let mut ephem_validator = start_ephem_validator();

        let test_cloning_dir =
            format!("{}/../{}", manifest_dir, "test-cloning");
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
    } else {
        let devnet_validator =
            config.setup_devnet(TEST_NAME).then(start_devnet_validator);
        let ephem_validator =
            config.setup_ephem(TEST_NAME).then(start_ephem_validator);
        wait_for_ctrlc(devnet_validator, ephem_validator, success_output())
    }
}

fn run_magicblock_api_tests(
    manifest_dir: &str,
    config: &TestConfigViaEnvVars,
) -> Result<Output, Box<dyn Error>> {
    const TEST_NAME: &str = "magicblock_api";
    if config.skip_entirely(TEST_NAME) {
        return Ok(success_output());
    }

    let start_devnet_validator = || match start_validator(
        "schedulecommit-conf.devnet.toml",
        ValidatorCluster::Chain(None),
        &LoadedAccounts::with_delegation_program_test_authority(),
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };

    let start_ephem_validator = || match start_validator(
        "validator-api-offline.devnet.toml",
        ValidatorCluster::Ephem,
        &LoadedAccounts::with_delegation_program_test_authority(),
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start ephemeral validator properly");
        }
    };

    if config.run_test(TEST_NAME) {
        eprintln!("======== RUNNING MAGICBLOCK API TESTS ========");

        let mut devnet_validator = start_devnet_validator();
        let mut ephem_validator = start_ephem_validator();

        let test_dir = format!("{}/../{}", manifest_dir, "test-magicblock-api");
        eprintln!("Running magicblock-api tests in {}", test_dir);

        let output = run_test(test_dir, Default::default()).map_err(|err| {
            eprintln!("Failed to magicblock api tests: {:?}", err);
            cleanup_validators(&mut ephem_validator, &mut devnet_validator);
            err
        })?;

        cleanup_validators(&mut ephem_validator, &mut devnet_validator);
        Ok(output)
    } else {
        let devnet_validator =
            config.setup_devnet(TEST_NAME).then(start_devnet_validator);
        let ephem_validator =
            config.setup_ephem(TEST_NAME).then(start_ephem_validator);
        wait_for_ctrlc(devnet_validator, ephem_validator, success_output())
    }
}

fn run_magicblock_pubsub_tests(
    manifest_dir: &str,
    config: &TestConfigViaEnvVars,
) -> Result<Output, Box<dyn Error>> {
    const TEST_NAME: &str = "pubsub";
    if config.skip_entirely(TEST_NAME) {
        return Ok(success_output());
    }

    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();

    let start_ephem_validator = || match start_validator(
        "validator-offline.devnet.toml",
        ValidatorCluster::Ephem,
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start ephemeral validator properly");
        }
    };

    if config.run_test(TEST_NAME) {
        eprintln!("======== RUNNING MAGICBLOCK PUBSUB TESTS ========");

        let mut ephem_validator = start_ephem_validator();

        let test_dir = format!("{}/../{}", manifest_dir, "test-pubsub");
        eprintln!("Running magicblock pubsub tests in {}", test_dir);

        let output = run_test(test_dir, Default::default()).map_err(|err| {
            eprintln!("Failed to magicblock pubsub tests: {:?}", err);
            cleanup_validator(&mut ephem_validator, "ephemeral");
            err
        })?;

        cleanup_validator(&mut ephem_validator, "ephemeral");
        Ok(output)
    } else {
        let ephem_validator =
            config.setup_ephem(TEST_NAME).then(start_ephem_validator);
        wait_for_ctrlc(None, ephem_validator, success_output())
    }
}

fn run_config_tests(
    manifest_dir: &str,
    config: &TestConfigViaEnvVars,
) -> Result<Output, Box<dyn Error>> {
    const TEST_NAME: &str = "config";
    if config.skip_entirely(TEST_NAME) {
        return Ok(success_output());
    }

    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();

    let start_devnet_validator = || match start_validator(
        "config-conf.devnet.toml",
        ValidatorCluster::Chain(Some(ProgramLoader::UpgradeableProgram)),
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };

    if config.run_test(TEST_NAME) {
        eprintln!("======== RUNNING CONFIG TESTS ========");

        let mut devnet_validator = start_devnet_validator();

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
    } else {
        let devnet_validator =
            config.setup_devnet(TEST_NAME).then(start_devnet_validator);
        wait_for_ctrlc(devnet_validator, None, success_output())
    }
}

fn run_schedule_intents_tests(
    manifest_dir: &str,
    config: &TestConfigViaEnvVars,
) -> Result<Output, Box<dyn Error>> {
    const TEST_NAME: &str = "schedule_intents";
    if config.skip_entirely(TEST_NAME) {
        return Ok(success_output());
    }

    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();
    let start_devnet_validator = || match start_validator(
        "config-conf.devnet.toml",
        ValidatorCluster::Chain(None),
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };
    let start_ephem_validator = || match start_validator(
        "schedulecommit-conf.ephem.frequent-commits.toml",
        ValidatorCluster::Ephem,
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start ephemeral validator properly");
        }
    };

    if config.run_test(TEST_NAME) {
        eprintln!("======== RUNNING ISSUES TESTS - Schedule Intents ========");

        let mut devnet_validator = start_devnet_validator();
        let mut ephem_validator = start_ephem_validator();

        let test_intents_dir =
            format!("{}/../{}", manifest_dir, "test-schedule-intent");
        eprintln!("Running schedule intents tests in {}", test_intents_dir);
        let test_output = match run_test(test_intents_dir, Default::default()) {
            Ok(output) => output,
            Err(err) => {
                eprintln!("Failed to run issues: {:?}", err);
                cleanup_validators(&mut ephem_validator, &mut devnet_validator);
                return Err(err.into());
            }
        };
        cleanup_validators(&mut ephem_validator, &mut devnet_validator);
        Ok(test_output)
    } else {
        let devnet_validator =
            config.setup_devnet(TEST_NAME).then(start_devnet_validator);
        let ephem_validator =
            config.setup_ephem(TEST_NAME).then(start_ephem_validator);
        wait_for_ctrlc(devnet_validator, ephem_validator, success_output())
    }
}

fn run_task_scheduler_tests(
    manifest_dir: &str,
    config: &TestConfigViaEnvVars,
) -> Result<Output, Box<dyn Error>> {
    const TEST_NAME: &str = "task-scheduler";
    if config.skip_entirely(TEST_NAME) {
        return Ok(success_output());
    }
    eprintln!("======== RUNNING TASK SCHEDULER TESTS ========");

    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();

    let start_devnet_validator = || match start_validator(
        "schedule-task.devnet.toml",
        ValidatorCluster::Chain(Some(ProgramLoader::UpgradeableProgram)),
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };

    let start_ephem_validator = || match start_validator(
        "schedule-task.ephem.toml",
        ValidatorCluster::Ephem,
        &loaded_chain_accounts,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start ephemeral validator properly");
        }
    };

    if config.run_test(TEST_NAME) {
        eprintln!("======== RUNNING TASK SCHEDULER TESTS ========");

        let mut devnet_validator = start_devnet_validator();
        let mut ephem_validator = start_ephem_validator();

        let test_dir = format!("{}/../{}", manifest_dir, "test-task-scheduler");
        eprintln!("Running task scheduler tests in {}", test_dir);

        let output = match run_test(test_dir, Default::default()) {
            Ok(output) => output,
            Err(err) => {
                eprintln!("Failed to run task scheduler tests: {:?}", err);
                cleanup_validators(&mut ephem_validator, &mut devnet_validator);
                return Err(err.into());
            }
        };

        cleanup_validators(&mut ephem_validator, &mut devnet_validator);
        Ok(output)
    } else {
        let devnet_validator =
            config.setup_devnet(TEST_NAME).then(start_devnet_validator);
        let ephem_validator =
            config.setup_ephem(TEST_NAME).then(start_ephem_validator);
        wait_for_ctrlc(devnet_validator, ephem_validator, success_output())
    }
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
