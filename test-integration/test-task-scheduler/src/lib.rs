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
        scheduler::TaskSchedulerConfig, validator::ValidatorConfig,
        LifecycleMode,
    },
    types::{crypto::SerdeKeypair, network::Remote, StorageDirectory},
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
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
    transaction::Transaction,
};
use tempfile::TempDir;

pub const TASK_SCHEDULER_TICK_MILLIS: u64 = 50;

fn validator_config(temp_dir: PathBuf, faucet: &Keypair) -> ValidatorParams {
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
        task_scheduler: TaskSchedulerConfig {
            faucet_keypair: Some(
                SerdeKeypair::from_str(&faucet.to_base58_string()).unwrap(),
            ),
        },
        storage: StorageDirectory(temp_dir),
        ..Default::default()
    }
}

fn airdrop_faucet(faucet: &Keypair) {
    let chain_ctx = IntegrationTestContext::try_new_chain_only()
        .expect("failed to connect to base chain to fund faucet");
    chain_ctx
        .airdrop_chain(&faucet.pubkey(), 100 * LAMPORTS_PER_SOL)
        .expect("failed to airdrop to task scheduler faucet");
}

fn start_validator(
    config: ValidatorParams,
    default_tmpdir: TempDir,
    temp_dir: PathBuf,
) -> (TempDir, Child, IntegrationTestContext, Option<Keypair>) {
    let faucet_keypair = config
        .task_scheduler
        .faucet_keypair
        .clone()
        .map(|k| k.insecure_clone());
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
    (default_tmpdir_config, validator, ctx, faucet_keypair)
}

pub fn setup_validator(
) -> (TempDir, Child, IntegrationTestContext, Option<Keypair>) {
    let (default_tmpdir, temp_dir) = resolve_tmp_dir(TMP_DIR_CONFIG);

    // The faucet that pays for cranks must be funded before the validator
    // delegates it on startup.
    let faucet = Keypair::new();
    airdrop_faucet(&faucet);

    let config = validator_config(temp_dir.clone(), &faucet);
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
        let client = expect!(ctx.try_ephem_client(), validator);
        let account = expect!(
            client.get_account_with_commitment(crank_pda, ctx.commitment),
            validator
        )
        .value;
        if account.is_none_or(|account| account.lamports == 0) {
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
