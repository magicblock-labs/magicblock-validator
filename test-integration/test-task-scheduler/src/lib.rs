use std::process::Child;

use integration_test_tools::{
    expect,
    loaded_accounts::LoadedAccounts,
    tmpdir::resolve_tmp_dir,
    validator::{
        start_magicblock_validator_with_config_struct_and_temp_dir,
        TMP_DIR_CONFIG,
    },
    IntegrationTestContext,
};
use magicblock_config::{
    AccountsConfig, EphemeralConfig, LedgerConfig, LedgerResumeStrategyConfig,
    LedgerResumeStrategyType, LifecycleMode, RemoteCluster, RemoteConfig,
    TaskSchedulerConfig,
};
use tempfile::TempDir;

pub fn setup_validator() -> (TempDir, Child, IntegrationTestContext) {
    let (default_tmpdir, temp_dir) = resolve_tmp_dir(TMP_DIR_CONFIG);
    let accounts_config = AccountsConfig {
        lifecycle: LifecycleMode::Ephemeral,
        remote: RemoteConfig {
            cluster: RemoteCluster::Custom,
            url: Some(IntegrationTestContext::url_chain().try_into().unwrap()),
            ws_url: Some(vec![IntegrationTestContext::ws_url_chain()
                .try_into()
                .unwrap()]),
        },
        ..Default::default()
    };

    let config = EphemeralConfig {
        accounts: accounts_config,
        task_scheduler: TaskSchedulerConfig {
            reset: true,
            millis_per_tick: 50,
        },
        ledger: LedgerConfig {
            resume_strategy_config: LedgerResumeStrategyConfig {
                kind: LedgerResumeStrategyType::Reset,
                ..Default::default()
            },
            path: temp_dir.to_string_lossy().to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let (default_tmpdir_config, Some(mut validator)) =
        start_magicblock_validator_with_config_struct_and_temp_dir(
            config,
            &LoadedAccounts::with_delegation_program_test_authority(),
            default_tmpdir,
            temp_dir,
        )
    else {
        panic!("validator should set up correctly");
    };

    let ctx = expect!(IntegrationTestContext::try_new(), validator);
    (default_tmpdir_config, validator, ctx)
}
