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
    // Initialize the counter
    let blockhash = expect!(
        ctx.try_chain_client().and_then(|client| client
            .get_latest_blockhash()
            .map_err(|e| anyhow::anyhow!(
                "Failed to get latest blockhash: {}",
                e
            ))),
        validator
    );
    expect!(
        ctx.send_transaction_chain(
            &mut Transaction::new_signed_with_payer(
                &[create_init_ix(payer.pubkey(), "test".to_string())],
                Some(&payer.pubkey()),
                &[&payer],
                blockhash,
            ),
            &[payer]
        ),
        format!("Failed to send init transaction: blockhash {:?}", blockhash),
        validator
    );

    // Delegate the counter to the ephem validator
    expect!(
        ctx.send_transaction_chain(
            &mut Transaction::new_signed_with_payer(
                &[create_delegate_ix_with_commit_frequency_ms(
                    payer.pubkey(),
                    commit_frequency_ms
                )],
                Some(&payer.pubkey()),
                &[&payer],
                blockhash,
            ),
            &[payer]
        ),
        validator
    );

    // Wait for account to be delegated
    expect!(ctx.wait_for_delta_slot_ephem(10), validator);
}

pub fn wait_for_incremented_counter(
    ctx: &IntegrationTestContext,
    counter_pda: &Pubkey,
    expected_count: u64,
    max_timeout: Duration,
    validator: &mut Child,
) {
    let now = Instant::now();
    while now.elapsed() < max_timeout {
        let counter_account = expect!(
            ctx.try_ephem_client().and_then(|client| client
                .get_account(counter_pda)
                .map_err(|e| anyhow::anyhow!("Failed to get account: {}", e))),
            validator
        );
        let counter =
            expect!(FlexiCounter::try_decode(&counter_account.data), validator);
        if counter.count == expected_count {
            return;
        }
        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }
    assert!(
        false,
        cleanup(validator),
        "Failed to wait for incremented counter"
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
    while now.elapsed() < max_timeout {
        let account = expect!(
            ctx.try_chain_client().and_then(|client| client
                .get_account(committee)
                .map_err(|e| anyhow::anyhow!(
                    "Failed to get chain account: {}",
                    e
                ))),
            validator
        );
        let state = expect!(MainAccount::try_decode(&account.data), validator);
        if state.count == expected_count {
            return;
        }
        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }
    assert!(
        false,
        cleanup(validator),
        "Timed out waiting for committed count {} on {}",
        expected_count,
        committee
    );
}
