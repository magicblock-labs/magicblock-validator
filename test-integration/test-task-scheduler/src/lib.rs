use std::{
    process::Child,
    str::FromStr,
    time::{Duration, Instant},
};

use cleanass::assert;
use integration_test_tools::{
    expect,
    loaded_accounts::LoadedAccounts,
    tmpdir::resolve_tmp_dir,
    validator::{
        cleanup, start_magicblock_validator_with_config_struct_and_temp_dir,
        TMP_DIR_CONFIG,
    },
    workspace_paths::path_relative_to_workspace,
    IntegrationTestContext,
};
use magicblock_config::{
    config::{LifecycleMode, LoadableProgram, TaskSchedulerConfig},
    types::{network::Remote, SerdePubkey},
    LeaderParams,
};
use magicblock_program::Pubkey;
use program_schedulecommit::MainAccount;
use tempfile::TempDir;

pub const TASK_SCHEDULER_TICK_MILLIS: u64 = 50;

pub fn setup_validator() -> (TempDir, Child, IntegrationTestContext) {
    let (default_tmpdir, temp_dir) = resolve_tmp_dir(TMP_DIR_CONFIG);
    let config = LeaderParams {
        lifecycle: LifecycleMode::Ephemeral,
        remotes: vec![
            Remote::from_str(IntegrationTestContext::url_chain()).unwrap(),
            Remote::from_str(IntegrationTestContext::ws_url_chain()).unwrap(),
        ],
        task_scheduler: TaskSchedulerConfig {
            reset: true,
            min_interval: Duration::from_millis(TASK_SCHEDULER_TICK_MILLIS),
            ..Default::default()
        },
        programs: vec![LoadableProgram {
            id: SerdePubkey(program_schedulecommit::ID),
            path: path_relative_to_workspace(
                "target/deploy/program_schedulecommit.so",
            )
            .into(),
        }],
        ..Default::default()
    };
    let (default_tmpdir_config, Some(mut validator), port) =
        start_magicblock_validator_with_config_struct_and_temp_dir(
            config,
            &LoadedAccounts::with_delegation_program_test_authority(),
            default_tmpdir,
            temp_dir,
        )
    else {
        panic!("validator should set up correctly");
    };
    let ctx = expect!(
        IntegrationTestContext::try_new_with_ephem_port(port),
        validator
    );
    (default_tmpdir_config, validator, ctx)
}

pub fn wait_for_committed_count(
    ctx: &IntegrationTestContext,
    committee: &Pubkey,
    expected_count: u64,
    max_timeout: Duration,
    validator: &mut Child,
) {
    let now = Instant::now();
    while now.elapsed() < max_timeout {
        let account = expect!(
            ctx.try_chain_client().and_then(|client| client
                .get_account(committee)
                .map_err(|err| anyhow::anyhow!(
                    "failed to get chain account: {err}"
                ))),
            validator
        );
        if let Ok(state) = MainAccount::try_decode(&account.data) {
            if state.count == expected_count {
                return;
            }
        }
        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }
    assert!(
        false,
        cleanup(validator),
        "task did not commit the expected count"
    );
}
