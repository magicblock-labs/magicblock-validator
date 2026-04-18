use std::{
    error::Error,
    io,
    path::Path,
    process::{self, Output},
    thread,
};

use integration_test_tools::{
    loaded_accounts::LoadedAccounts,
    toml_to_args::{rpc_port_from_config, ProgramLoader},
    validator::{
        resolve_workspace_dir, start_magic_block_validator_with_config,
        start_test_validator_with_config, TestRunnerPaths,
    },
};
use teepee::Teepee;
use test_runner::{
    cleanup::{cleanup_devnet_only, cleanup_validators},
    env_config::TestConfigViaEnvVars,
    signal::wait_for_ctrlc,
};
use test_runner::cleanup::kill_validators;

// Helper macro that registers a list of tests executed each on a separate thread
macro_rules! register_tests {
    ($scope:expr, ($($arg:expr),* $(,)?), { $($rule:tt)* }) => {{
        let mut handles = Vec::new();
        register_tests!(@rules $scope, ($($arg),*), handles, $($rule)*);
        handles
    }};

    (@rules $scope:expr, $args:tt, $handles:ident,) => {};

    // Single-label: function returns `Result<Output, _>`.
    (@rules $scope:expr, ($($arg:expr),*), $handles:ident,
        $func:path => $label:ident $(, $($rest:tt)*)?
    ) => {
        $handles.push($scope.spawn(|| {
            let out = $func($($arg),*)
                .expect(concat!(stringify!($label), " panicked"));
            vec![(stringify!($label), out)]
        }));
        register_tests!(@rules $scope, ($($arg),*), $handles, $($($rest)*)?);
    };

    // Tuple-label: function returns `Result<(Output, Output, ...), _>`.
    (@rules $scope:expr, ($($arg:expr),*), $handles:ident,
        $func:path => ($($label:ident),+ $(,)?) $(, $($rest:tt)*)?
    ) => {
        $handles.push($scope.spawn(|| {
            let ($($label),+) = $func($($arg),*)
                .expect(concat!(stringify!($func), " panicked"));
            vec![$((stringify!($label), $label)),+]
        }));
        register_tests!(@rules $scope, ($($arg),*), $handles, $($($rest)*)?);
    };
}

struct ValidatorCleanupGuard;

impl Drop for ValidatorCleanupGuard {
    fn drop(&mut self) {
        kill_validators();
    }
}

pub fn main() {
    let config = TestConfigViaEnvVars::default();
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    let _validator_cleanup = ValidatorCleanupGuard;

    let results: Vec<(&'static str, Output)> = thread::scope(|s| {
        let handles = register_tests!(s, (&manifest_dir, &config), {
            run_schedule_commit_tests => (security, scenarios),
            run_chainlink_tests => chainlink,
            run_cloning_tests => cloning,
            run_restore_ledger_tests => restore_ledger,
            run_magicblock_api_tests => magicblock_api,
            run_table_mania_and_committor_tests => (table_mania, committor),
            run_magicblock_pubsub_tests => magicblock_pubsub,
            run_config_tests => config,
            run_schedule_intents_tests => schedule_intents,
            run_task_scheduler_tests => task_scheduler,
        });

        handles
            .into_iter()
            .flat_map(|h| h.join().expect("handler thread panicked"))
            .collect()
    });

    for (name, output) in results {
        assert_cargo_tests_passed(output, name);
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
        TEST_NAME,
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
        let output = match run_test(
            test_restore_ledger_dir,
            RunTestConfig {
                chain_config: Some("restore-ledger-conf.devnet.toml"),
                ..Default::default()
            },
        ) {
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

fn run_chainlink_tests(
    manifest_dir: &str,
    config: &TestConfigViaEnvVars,
) -> Result<Output, Box<dyn Error>> {
    const TEST_NAME: &str = "chainlink";
    if config.skip_entirely(TEST_NAME) {
        return Ok(success_output());
    }
    let loaded_chain_accounts = {
        let mut loaded_chain_accounts =
            LoadedAccounts::with_delegation_program_test_authority();
        loaded_chain_accounts.add(&[
            (
                "Memo1UhkJRfHyvLMcVucJwxXeuD728EqVDDwQDxFMNo",
                "memo_v1.json",
            ),
            (
                "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr",
                "memo_v2.json",
            ),
            (
                "BL5oAaURQwAVVHcgrucxJe3H5K57kCQ5Q8ys7dctqfV8",
                "old_program_v1.json",
            ),
            (
                "MiniV21111111111111111111111111111111111111",
                "target/deploy/miniv2/program_mini.json",
            ),
        ]);
        loaded_chain_accounts
    };
    let start_devnet_validator = || match start_validator(
        "chainlink-conf.devnet.toml",
        ValidatorCluster::Chain(None),
        &loaded_chain_accounts,
        TEST_NAME,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };
    if config.run_test(TEST_NAME) {
        eprintln!("======== RUNNING CHAINLINK TESTS ========");
        let mut devnet_validator = start_devnet_validator();
        let test_chainlink_dir =
            format!("{}/../{}", manifest_dir, "test-chainlink");
        eprintln!("Running chainlink tests in {}", test_chainlink_dir);
        let output = match run_test(
            test_chainlink_dir,
            RunTestConfig {
                chain_config: Some("chainlink-conf.devnet.toml"),
                ..Default::default()
            },
        ) {
            Ok(output) => output,
            Err(err) => {
                eprintln!("Failed to run chainlink tests: {:?}", err);
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
        COMMITTOR_TEST,
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

            match run_test(
                test_table_mania_dir,
                RunTestConfig {
                    chain_config: Some("committor-conf.devnet.toml"),
                    ..Default::default()
                },
            ) {
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
            match run_test(
                test_committor_dir,
                RunTestConfig {
                    chain_config: Some("committor-conf.devnet.toml"),
                    ..Default::default()
                },
                // RunTestConfig {
                //     package: Some("schedulecommit-committor-service"),
                //     test_file: Some("test_ix_commit_local"),
                //     test_fn_name: Some(
                //         "test_ix_execute_intent_bundle_commit_and_commit_finalize_mixed",
                //         //"test_ix_execute_intent_bundle_commit_and_cau_simultaneously_union_of_accounts",
                //     ),
                // },
            ) {
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
        TEST_NAME,
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
        TEST_NAME,
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
        let test_security_output = match run_test(
            test_security_dir,
            RunTestConfig {
                chain_config: Some("schedulecommit-conf.devnet.toml"),
                ephem_config: Some("schedulecommit-conf-fees.ephem.toml"),
                ..Default::default()
            },
        ) {
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
        let test_scenarios_output = match run_test(
            test_scenarios_dir,
            RunTestConfig {
                chain_config: Some("schedulecommit-conf.devnet.toml"),
                ephem_config: Some("schedulecommit-conf-fees.ephem.toml"),
                ..Default::default()
            },
        ) {
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

fn run_cloning_tests(
    manifest_dir: &str,
    config: &TestConfigViaEnvVars,
) -> Result<Output, Box<dyn Error>> {
    const TEST_NAME: &str = "cloning";
    if config.skip_entirely(TEST_NAME) {
        return Ok(success_output());
    }

    let loaded_chain_accounts = {
        let mut loaded_chain_accounts =
            LoadedAccounts::with_delegation_program_test_authority();

        loaded_chain_accounts.add(&[
            (
                "Memo1UhkJRfHyvLMcVucJwxXeuD728EqVDDwQDxFMNo",
                "memo_v1.json",
            ),
            (
                "MiniV21111111111111111111111111111111111111",
                "target/deploy/miniv2/program_mini.json",
            ),
        ]);
        loaded_chain_accounts
    };
    let start_devnet_validator = || match start_validator(
        "cloning-conf.devnet.toml",
        ValidatorCluster::Chain(Some(ProgramLoader::UpgradeableProgram)),
        &loaded_chain_accounts,
        TEST_NAME,
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
        TEST_NAME,
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
        let output = match run_test(
            test_cloning_dir,
            RunTestConfig {
                chain_config: Some("cloning-conf.devnet.toml"),
                ephem_config: Some("cloning-conf.ephem.toml"),
                ..Default::default()
            },
            // RunTestConfig {
            //     package: Some("test-cloning"),
            //     test_file: Some("10_post_delegation_token_transfer"),
            //     test_fn_name: None,
            //     ..Default::default()
            // },
        ) {
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
        "magicblock-api-chain.devnet.toml",
        ValidatorCluster::Chain(None),
        &LoadedAccounts::with_delegation_program_test_authority(),
        TEST_NAME,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };

    let start_ephem_validator = || match start_validator(
        "api-conf.ephem.toml",
        ValidatorCluster::Ephem,
        &LoadedAccounts::with_delegation_program_test_authority(),
        TEST_NAME,
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

        let output = run_test(
            test_dir,
            RunTestConfig {
                chain_config: Some("magicblock-api-chain.devnet.toml"),
                ephem_config: Some("api-conf.ephem.toml"),
                ..Default::default()
            },
        )
        .map_err(|err| {
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

    let start_devnet_validator = || match start_validator(
        "pubsub-chain.devnet.toml",
        ValidatorCluster::Chain(None),
        &loaded_chain_accounts,
        TEST_NAME,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start ephemeral validator properly");
        }
    };
    let start_ephem_validator = || match start_validator(
        "pubsub-ephem.toml",
        ValidatorCluster::Ephem,
        &loaded_chain_accounts,
        TEST_NAME,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start ephemeral validator properly");
        }
    };

    if config.run_test(TEST_NAME) {
        eprintln!("======== RUNNING MAGICBLOCK PUBSUB TESTS ========");

        let mut devnet_validator = start_devnet_validator();
        let mut ephem_validator = start_ephem_validator();

        let test_dir = format!("{}/../{}", manifest_dir, "test-pubsub");
        eprintln!("Running magicblock pubsub tests in {}", test_dir);

        let output = run_test(
            test_dir,
            RunTestConfig {
                chain_config: Some("pubsub-chain.devnet.toml"),
                ephem_config: Some("pubsub-ephem.toml"),
                ..Default::default()
            },
        )
        .map_err(|err| {
            eprintln!("Failed to magicblock pubsub tests: {:?}", err);
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
        TEST_NAME,
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
        let output = match run_test(
            test_config_dir,
            RunTestConfig {
                chain_config: Some("config-conf.devnet.toml"),
                ..Default::default()
            },
        ) {
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
        "schedule-intents-chain.devnet.toml",
        ValidatorCluster::Chain(None),
        &loaded_chain_accounts,
        TEST_NAME,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };
    let start_ephem_validator = || match start_validator(
        "schedule-intents-ephem.toml",
        ValidatorCluster::Ephem,
        &loaded_chain_accounts,
        TEST_NAME,
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
        let test_output = match run_test(
            test_intents_dir,
            RunTestConfig {
                chain_config: Some("schedule-intents-chain.devnet.toml"),
                ephem_config: Some("schedule-intents-ephem.toml"),
                ..Default::default()
            },
            // RunTestConfig {
            //     package: Some("test-schedule-intent"),
            //     test_file: Some("test_schedule_intents"),
            //     test_fn_name: Some(
            //         "test_intent_bundle_commit_and_commit_finalize",
            //     ),
            //     ..Default::default()
            // },
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

    let loaded_chain_accounts =
        LoadedAccounts::with_delegation_program_test_authority();

    let start_devnet_validator = || match start_validator(
        "schedule-task.devnet.toml",
        ValidatorCluster::Chain(Some(ProgramLoader::UpgradeableProgram)),
        &loaded_chain_accounts,
        TEST_NAME,
    ) {
        Some(validator) => validator,
        None => {
            panic!("Failed to start devnet validator properly");
        }
    };

    if config.run_test(TEST_NAME) {
        eprintln!("======== RUNNING TASK SCHEDULER TESTS ========");

        let mut devnet_validator = start_devnet_validator();

        let test_dir = format!("{}/../{}", manifest_dir, "test-task-scheduler");
        eprintln!("Running task scheduler tests in {}", test_dir);

        let output = match run_test(
            test_dir,
            RunTestConfig {
                chain_config: Some("schedule-task.devnet.toml"),
                ..Default::default()
            },
        ) {
            Ok(output) => output,
            Err(err) => {
                eprintln!("Failed to run task scheduler tests: {:?}", err);
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
    test_file: Option<&'a str>,
    test_fn_name: Option<&'a str>,
    /// Chain TOML config filename (relative to configs/). When set,
    /// `[aperture].listen` is resolved and exported as CHAIN_URL/CHAIN_WS_URL
    /// on the cargo test child process so tests reach the correct port
    /// without racing on process-wide env vars.
    chain_config: Option<&'a str>,
    /// Ephem TOML config filename (relative to configs/). Resolved into
    /// EPHEM_URL/EPHEM_WS_URL on the cargo test child process.
    ephem_config: Option<&'a str>,
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
    if let Some(chain_config) = config.chain_config {
        let port = rpc_port_from_config(&resolve_paths(chain_config).config_path);
        cmd.env("CHAIN_URL", format!("http://127.0.0.1:{port}"))
            .env("CHAIN_WS_URL", format!("ws://127.0.0.1:{}", port + 1));
    }
    if let Some(ephem_config) = config.ephem_config {
        let port = rpc_port_from_config(&resolve_paths(ephem_config).config_path);
        cmd.env("EPHEM_URL", format!("http://127.0.0.1:{port}"))
            .env("EPHEM_WS_URL", format!("ws://127.0.0.1:{}", port + 1));
    }
    if let Some(package) = config.package {
        cmd.arg("-p").arg(package);
    }
    if let Some(test_file) = config.test_file {
        cmd.arg("--test").arg(test_file);
    }
    if let Some(test_fn_name) = config.test_fn_name {
        cmd.arg(test_fn_name);
    }
    cmd.arg("--").arg("--test-threads=1").arg("--nocapture");
    cmd.current_dir(manifest_dir.clone());
    println!("RUNNING: {:?}", cmd);
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
    suite_name: &str,
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
                suite_name,
            )
        }
        _ => start_magic_block_validator_with_config(
            &test_runner_paths,
            log_suffix,
            loaded_chain_accounts,
        ),
    }
}
