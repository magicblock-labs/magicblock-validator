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
    IntegrationTestContext,
};
use magicblock_config::{
    config::{
        accounts::AccountsDbConfig, ledger::LedgerConfig,
        scheduler::TaskSchedulerConfig, validator::ValidatorConfig,
        LifecycleMode,
    },
    types::{network::Remote, StorageDirectory},
    ValidatorParams,
};
use magicblock_program::Pubkey;
use program_flexi_counter::{
    instruction::{
        create_delegate_ix_with_commit_frequency_ms, create_init_ix,
    },
    state::FlexiCounter,
};
use program_schedulecommit::MainAccount;
use solana_sdk::{
    signature::Keypair, signer::Signer, transaction::Transaction,
};
use tempfile::TempDir;

pub const TASK_SCHEDULER_TICK_MILLIS: u64 = 50;

pub fn setup_validator() -> (TempDir, Child, IntegrationTestContext) {
    let (default_tmpdir, temp_dir) = resolve_tmp_dir(TMP_DIR_CONFIG);

    let config = ValidatorParams {
        lifecycle: LifecycleMode::Ephemeral,
        remotes: vec![
            Remote::from_str(IntegrationTestContext::url_chain()).unwrap(),
            Remote::from_str(IntegrationTestContext::ws_url_chain()).unwrap(),
        ],
        accountsdb: AccountsDbConfig::default(),
        task_scheduler: TaskSchedulerConfig {
            reset: true,
            ..Default::default()
        },
        validator: ValidatorConfig {
            ..Default::default()
        },
        ledger: LedgerConfig {
            reset: true,
            block_time: Duration::from_millis(TASK_SCHEDULER_TICK_MILLIS),
            ..Default::default()
        },
        storage: StorageDirectory(temp_dir.clone()),
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

pub fn create_delegated_counter(
    ctx: &IntegrationTestContext,
    payer: &Keypair,
    validator: &mut Child,
    commit_frequency_ms: u32,
) {
    let init_ix = create_init_ix(payer.pubkey(), "test".to_string());
    let delegate_ix = create_delegate_ix_with_commit_frequency_ms(
        payer.pubkey(),
        commit_frequency_ms,
    );

    expect!(
        ctx.send_and_confirm_instructions_with_payer_chain(
            &[init_ix, delegate_ix],
            payer,
        ),
        validator
    );

    let (counter_pda, _) = FlexiCounter::pda(&payer.pubkey());
    wait_for_incremented_counter(
        ctx,
        &counter_pda,
        0,
        Duration::from_secs(10),
        validator,
    );
}

pub fn wait_for_incremented_counter(
    ctx: &IntegrationTestContext,
    counter_pda: &Pubkey,
    expected_count: u64,
    max_timeout: Duration,
    validator: &mut Child,
) {
    let now = Instant::now();
    let mut last_count = None;
    let mut last_error = None;
    while now.elapsed() < max_timeout {
        let counter = ctx
            .try_ephem_client()
            .and_then(|client| {
                client.get_account(counter_pda).map_err(|e| {
                    anyhow::anyhow!("Failed to get account: {}", e)
                })
            })
            .and_then(|counter_account| {
                FlexiCounter::try_decode(&counter_account.data).map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to decode counter account ({} bytes): {}",
                        counter_account.data.len(),
                        e
                    )
                })
            });
        match counter {
            Ok(counter) => {
                last_count = Some(counter.count);
                last_error = None;
                if counter.count == expected_count {
                    return;
                }
            }
            Err(err) => {
                last_error = Some(err.to_string());
            }
        }
        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }
    assert!(
        false,
        cleanup(validator),
        "Failed to wait for incremented counter; expected_count: {}, last_count: {:?}, last_error: {:?}",
        expected_count,
        last_count,
        last_error
    );
}

pub fn wait_for_committed_count(
    ctx: &IntegrationTestContext,
    committee: &Pubkey,
    expected_count: u64,
    max_timeout: Duration,
    validator: &mut Child,
) {
    let now = Instant::now();
    let mut last_count = None;
    let mut last_error = None;
    while now.elapsed() < max_timeout {
        let state = ctx
            .try_chain_client()
            .and_then(|client| {
                client.get_account(committee).map_err(|e| {
                    anyhow::anyhow!("Failed to get chain account: {}", e)
                })
            })
            .and_then(|account| {
                MainAccount::try_decode(&account.data).map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to decode chain account ({} bytes): {}",
                        account.data.len(),
                        e
                    )
                })
            });
        match state {
            Ok(state) => {
                last_count = Some(state.count);
                last_error = None;
                if state.count == expected_count {
                    return;
                }
            }
            Err(err) => {
                last_error = Some(err.to_string());
            }
        }
        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }
    assert!(
        false,
        cleanup(validator),
        "Timed out waiting for committed count {} on {}; last_count: {:?}, last_error: {:?}",
        expected_count,
        committee,
        last_count,
        last_error
    );
}
