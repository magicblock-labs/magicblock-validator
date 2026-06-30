use std::{
    path::PathBuf,
    process::Child,
    str::FromStr,
    time::{Duration, Instant},
};

use cleanass::assert;
use hydra_api::ephemeral::ID as HYDRA_EPHEMERAL_PROGRAM_ID;
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
        validator::ValidatorConfig, LifecycleMode,
    },
    types::{network::Remote, StorageDirectory},
    ValidatorParams,
};
use magicblock_program::Pubkey;
use magicblock_task_scheduler::{db::DbTask, SchedulerDatabase};
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
use tokio::runtime::Runtime;

pub const TASK_SCHEDULER_TICK_MILLIS: u64 = 50;

fn validator_config(temp_dir: PathBuf) -> ValidatorParams {
    ValidatorParams {
        lifecycle: LifecycleMode::Ephemeral,
        remotes: vec![
            Remote::from_str(IntegrationTestContext::url_chain()).unwrap(),
            Remote::from_str(IntegrationTestContext::ws_url_chain()).unwrap(),
        ],
        accountsdb: AccountsDbConfig::default(),
        validator: ValidatorConfig {
            ..Default::default()
        },
        ledger: LedgerConfig {
            reset: true,
            block_time: Duration::from_millis(TASK_SCHEDULER_TICK_MILLIS),
            ..Default::default()
        },
        storage: StorageDirectory(temp_dir),
        ..Default::default()
    }
}

fn start_validator(
    config: ValidatorParams,
    default_tmpdir: TempDir,
    temp_dir: PathBuf,
) -> (TempDir, Child, IntegrationTestContext) {
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

pub fn setup_validator() -> (TempDir, Child, IntegrationTestContext) {
    setup_validator_with_migration_tasks(&[])
}

pub fn setup_validator_with_migration_tasks(
    tasks: &[DbTask],
) -> (TempDir, Child, IntegrationTestContext) {
    let (default_tmpdir, temp_dir) = resolve_tmp_dir(TMP_DIR_CONFIG);

    // Seed the migration database before the validator opens it. The handle is
    // dropped (flushing the WAL) before startup.
    {
        let db_path = SchedulerDatabase::path(&temp_dir);
        let db = SchedulerDatabase::new(&db_path)
            .expect("failed to open seed database");
        let runtime = Runtime::new().expect("failed to create runtime");
        for task in tasks {
            runtime
                .block_on(db.insert_task(task))
                .expect("failed to seed task");
        }
    }

    let config = validator_config(temp_dir.clone());
    start_validator(config, default_tmpdir, temp_dir)
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

pub fn wait_for_hydra_crank(
    ctx: &IntegrationTestContext,
    crank_pda: &Pubkey,
    expected_lamports: u64,
    max_timeout: Duration,
    validator: &mut Child,
) {
    let now = Instant::now();
    while now.elapsed() < max_timeout {
        let maybe_account = ctx
            .try_ephem_client()
            .ok()
            .and_then(|client| client.get_account(crank_pda).ok());
        if let Some(account) = maybe_account {
            assert!(
                account.owner.to_bytes()
                    == HYDRA_EPHEMERAL_PROGRAM_ID.to_bytes(),
                cleanup(validator),
                "crank account {} not owned by hydra program (owner: {})",
                crank_pda,
                account.owner
            );
            assert!(
                account.lamports >= expected_lamports,
                cleanup(validator),
                "crank account {} underfunded: {} < {}",
                crank_pda,
                account.lamports,
                expected_lamports
            );
            return;
        }
        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }
    assert!(
        false,
        cleanup(validator),
        "hydra crank account {} was not created before timeout", crank_pda
    );
}

pub fn wait_for_hydra_crank_closed(
    ctx: &IntegrationTestContext,
    crank_pda: &Pubkey,
    max_timeout: Duration,
    validator: &mut Child,
) {
    let now = Instant::now();
    while now.elapsed() < max_timeout {
        let closed = match ctx
            .try_ephem_client()
            .ok()
            .and_then(|client| client.get_account(crank_pda).ok())
        {
            // Account fully removed.
            None => true,
            // Closed ephemeral accounts are drained to zero lamports.
            Some(account) => account.lamports == 0,
        };
        if closed {
            return;
        }
        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }
    assert!(
        false,
        cleanup(validator),
        "hydra crank account {} was not closed before timeout", crank_pda
    );
}

pub fn wait_for_empty_db(
    runtime: &Runtime,
    db: &SchedulerDatabase,
    max_timeout: Duration,
    validator: &mut Child,
) {
    let now = Instant::now();
    while now.elapsed() < max_timeout {
        let ids = expect!(runtime.block_on(db.get_task_ids()), validator);
        if ids.is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        false,
        cleanup(validator),
        "migration database was not emptied before timeout"
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
    let mut last_status: Option<String> = None;
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
        match MainAccount::try_decode(&account.data) {
            Ok(state) => {
                if state.count == expected_count {
                    return;
                }
                last_status = Some(format!(
                    "committee={} observed_count={} expected_count={}",
                    committee, state.count, expected_count
                ));
            }
            Err(err) => {
                eprintln!(
                    "Failed to decode committed account for committee {}: {} (data_len={})",
                    committee,
                    err,
                    account.data.len()
                );
                last_status = Some(format!(
                    "committee={} decode_error={} data_len={} expected_count={}",
                    committee,
                    err,
                    account.data.len(),
                    expected_count
                ));
            }
        }
        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }
    assert!(
        false,
        cleanup(validator),
        "Timed out waiting to observe committed count {} for committee {} before timeout; last observed status: {}",
        expected_count,
        committee,
        last_status
            .as_deref()
            .unwrap_or("no successful poll result recorded")
    );
}
