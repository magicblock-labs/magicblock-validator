use std::{process::Child, time::Duration};

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
    config::{
        accounts::AccountsDbConfig, ledger::LedgerConfig,
        scheduler::TaskSchedulerConfig, validator::ValidatorConfig,
        LifecycleMode,
    },
    types::{
        network::{Remote, RemoteCluster},
        StorageDirectory,
    },
    ValidatorParams,
};
use program_flexi_counter::instruction::{
    create_delegate_ix_with_commit_frequency_ms, create_init_ix,
};
use solana_sdk::{
    signature::Keypair, signer::Signer, transaction::Transaction,
};
use tempfile::TempDir;

pub const TASK_SCHEDULER_TICK_MILLIS: u64 = 50;

pub fn setup_validator() -> (TempDir, Child, IntegrationTestContext) {
    let (default_tmpdir, temp_dir) = resolve_tmp_dir(TMP_DIR_CONFIG);

    let config = ValidatorParams {
        lifecycle: LifecycleMode::Ephemeral,
        remote: RemoteCluster::Single(Remote::Disjointed {
            http: IntegrationTestContext::url_chain().parse().unwrap(),
            ws: IntegrationTestContext::ws_url_chain().parse().unwrap(),
        }),
        accountsdb: AccountsDbConfig::default(),
        task_scheduler: TaskSchedulerConfig { reset: true },
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
