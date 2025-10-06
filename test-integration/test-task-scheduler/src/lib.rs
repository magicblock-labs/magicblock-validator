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
    TaskSchedulerConfig, ValidatorConfig,
};
use program_flexi_counter::instruction::{create_delegate_ix, create_init_ix};
use solana_sdk::{
    hash::Hash, instruction::Instruction, pubkey::Pubkey, signature::Keypair,
    signer::Signer, transaction::Transaction,
};
use tempfile::TempDir;

pub const NOOP_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV");

pub const TASK_SCHEDULER_TICK_MILLIS: u64 = 50;

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
            millis_per_tick: TASK_SCHEDULER_TICK_MILLIS,
        },
        validator: ValidatorConfig {
            millis_per_slot: TASK_SCHEDULER_TICK_MILLIS,
            ..Default::default()
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

pub fn create_delegated_counter(
    ctx: &IntegrationTestContext,
    payer: &Keypair,
    validator: &mut Child,
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
        validator
    );

    // Delegate the counter to the ephem validator
    expect!(
        ctx.send_transaction_chain(
            &mut Transaction::new_signed_with_payer(
                &[create_delegate_ix(payer.pubkey())],
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

pub fn send_noop_tx(
    ctx: &IntegrationTestContext,
    payer: &Keypair,
    validator: &mut Child,
) -> Hash {
    // Noop tx to make sure the noop program is cloned
    let ephem_blockhash = expect!(
        ctx.try_ephem_client().and_then(|client| client
            .get_latest_blockhash()
            .map_err(|e| anyhow::anyhow!(
                "Failed to get latest blockhash: {}",
                e
            ))),
        validator
    );
    let noop_instruction =
        Instruction::new_with_bytes(NOOP_PROGRAM_ID, &[0], vec![]);
    expect!(
        ctx.send_transaction_ephem(
            &mut Transaction::new_signed_with_payer(
                &[noop_instruction],
                Some(&payer.pubkey()),
                &[&payer],
                ephem_blockhash,
            ),
            &[payer]
        ),
        validator
    );

    ephem_blockhash
}
