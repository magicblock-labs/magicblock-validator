use std::{
    fs,
    path::Path,
    process::{self, Child},
};

use integration_test_tools::{
    expect,
    loaded_accounts::LoadedAccounts,
    tmpdir::resolve_tmp_dir,
    validator::{resolve_workspace_dir, TestRunnerPaths},
    IntegrationTestContext,
};
use test_ledger_restore::start_validator_with_config;
use magicblock_config::{
    AccountsCloneConfig, AccountsConfig, EphemeralConfig, LifecycleMode,
    PrepareLookupTables, RemoteCluster, RemoteConfig,
};
use program_flexi_counter::{
    instruction::{create_add_ix, create_delegate_ix, create_init_ix},
    state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer,
};
use tempfile::TempDir;

pub const TMP_DIR_CONFIG: &str = "TMP_DIR_CONFIG";

/// Starts a validator with the given clone configuration
pub fn start_validator_with_clone_config(
    prepare_lookup_tables: PrepareLookupTables,
    loaded_chain_accounts: &LoadedAccounts,
) -> (TempDir, Child, IntegrationTestContext) {
    let workspace_dir = resolve_workspace_dir();
    let (default_tmpdir, temp_dir) = resolve_tmp_dir(TMP_DIR_CONFIG);
    let release = std::env::var("RELEASE").is_ok();
    let config_path = temp_dir.join("config.toml");

    // Create config with specific clone setting
    let config = EphemeralConfig {
        accounts: AccountsConfig {
            remote: RemoteConfig {
                cluster: RemoteCluster::Custom,
                url: Some(
                    IntegrationTestContext::url_chain().try_into().unwrap(),
                ),
                ws_url: None,
            },
            lifecycle: LifecycleMode::Ephemeral,
            clone: AccountsCloneConfig {
                prepare_lookup_tables,
            },
            ..Default::default()
        },
        ..Default::default()
    };

    let config_toml = config.to_string();
    fs::write(&config_path, config_toml).unwrap();

    let root_dir = Path::new(&workspace_dir)
        .join("..")
        .canonicalize()
        .unwrap()
        .to_path_buf();
    let paths = TestRunnerPaths {
        config_path,
        root_dir,
        workspace_dir,
    };

    let (default_tmpdir_config, Some(validator)) =
        start_validator_with_config(
            config,
            &LoadedAccounts::with_delegation_program_test_authority(),
        )
    else {
        panic!("validator should set up correctly");
    };

    let ctx = expect!(IntegrationTestContext::try_new(), validator);
    (default_tmpdir_config, validator, ctx)
}

pub fn cleanup(validator: &mut Child) {
    let _ = validator.kill().inspect_err(|e| {
        eprintln!("ERR: Failed to kill validator: {:?}", e);
    });
}

/// Wait for the validator to start up properly
pub fn wait_for_startup(validator: &mut Child) {
    let ctx = expect!(IntegrationTestContext::try_new_ephem_only(), validator);
    // Wait for at least one slot to advance to ensure the validator is running
    expect!(ctx.wait_for_next_slot_ephem(), validator);
}

/// Create an account on chain, delegate it, and send a transaction to ephemeral validator to trigger cloning
pub fn delegate_and_clone(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
) -> Keypair {
    let payer = Keypair::new();

    // 1. Airdrop to payer on chain
    expect!(
        ctx.airdrop_chain(&payer.pubkey(), LAMPORTS_PER_SOL),
        validator
    );

    // 2. Create and send init counter instruction on chain
    let init_ix = create_init_ix(payer.pubkey(), "TEST_COUNTER".to_string());
    expect!(
        ctx.send_transaction_with_payer_chain(&init_ix, &payer),
        validator
    );

    // 3. Delegate counter to ephemeral
    let delegate_ix = create_delegate_ix(payer.pubkey());
    expect!(
        ctx.send_transaction_with_payer_chain(&delegate_ix, &payer),
        validator
    );

    // 4. Send a transaction to ephemeral validator to trigger cloning
    let add_ix = create_add_ix(payer.pubkey(), 1);
    expect!(
        ctx.send_transaction_with_payer_ephem(&add_ix, &payer),
        validator
    );

    payer
}
