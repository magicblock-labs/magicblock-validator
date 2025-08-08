use std::process::Child;

use integration_test_tools::{
    expect,
    loaded_accounts::LoadedAccounts,
    validator::{
        resolve_programs, start_magicblock_validator_with_config_struct,
    },
    IntegrationTestContext,
};
use magicblock_config::{
    AccountsCloneConfig, AccountsConfig, EphemeralConfig, LifecycleMode,
    PrepareLookupTables, ProgramConfig, RemoteCluster, RemoteConfig,
};
use program_flexi_counter::instruction::{
    create_add_ix, create_delegate_ix, create_init_ix,
};
use solana_sdk::{
    address_lookup_table, native_token::LAMPORTS_PER_SOL, signature::Keypair,
    signer::Signer,
};
use tempfile::TempDir;

fn get_programs() -> Vec<ProgramConfig> {
    let flexicounter_id = program_flexi_counter::id();
    resolve_programs(Some(vec![ProgramConfig {
        id: flexicounter_id,
        path: "program_flexi_counter.so".to_string(),
    }]))
}

/// Starts a validator with the given clone configuration
pub fn start_validator_with_clone_config(
    prepare_lookup_tables: PrepareLookupTables,
    loaded_chain_accounts: &LoadedAccounts,
) -> (TempDir, Child, IntegrationTestContext) {
    let programs = get_programs();

    let config = EphemeralConfig {
        programs,
        accounts: AccountsConfig {
            remote: RemoteConfig {
                cluster: RemoteCluster::Custom,
                url: Some(
                    IntegrationTestContext::url_chain().try_into().unwrap(),
                ),
                ws_url: Some(vec![IntegrationTestContext::ws_url_chain()
                    .try_into()
                    .unwrap()]),
            },

            lifecycle: LifecycleMode::Ephemeral,
            clone: AccountsCloneConfig {
                prepare_lookup_tables,
                auto_airdrop_lamports: 0
            },
            ..Default::default()
        },
        ..Default::default()
    };

    let (default_tmpdir, Some(mut validator)) =
        start_magicblock_validator_with_config_struct(
            config,
            loaded_chain_accounts,
        )
    else {
        panic!("validator should set up correctly");
    };

    let ctx = expect!(IntegrationTestContext::try_new(), validator);
    (default_tmpdir, validator, ctx)
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

    // 2. Create and send init counter instruction on chain and delegate it
    let init_ix = create_init_ix(payer.pubkey(), "TEST_COUNTER".to_string());
    let delegate_ix = create_delegate_ix(payer.pubkey());
    expect!(
        ctx.send_and_confirm_instructions_with_payer_chain(
            &[init_ix, delegate_ix],
            &payer
        ),
        validator
    );

    // 3. Send a transaction to ephemeral validator to trigger cloning
    let add_ix = create_add_ix(payer.pubkey(), 1);
    expect!(
        ctx.send_and_confirm_instructions_with_payer_ephem(&[add_ix], &payer),
        validator
    );

    payer
}

pub fn count_lookup_table_transactions_on_chain(
    ctx: &IntegrationTestContext,
) -> anyhow::Result<usize> {
    let lookup_table_program_id = address_lookup_table::program::id();

    let sigs =
        ctx.get_signaturestats_for_address_chain(&lookup_table_program_id)?;

    Ok(sigs.len())
}
