use std::str::FromStr;

use cleanass::{assert, assert_eq};
use integration_test_tools::{
    expect,
    loaded_accounts::LoadedAccounts,
    validator::{cleanup, start_magicblock_validator_with_config_struct},
    IntegrationTestContext,
};
use magicblock_config::{
    config::{
        accounts::AccountsDbConfig, chain::ChainLinkConfig,
        ledger::LedgerConfig, LifecycleMode,
    },
    types::network::Remote,
    ValidatorParams,
};
use serial_test::file_serial;
use solana_sdk::{signature::Keypair, signer::Signer, system_instruction};
use test_kit::init_logger;

#[test]
#[file_serial]
fn test_auto_airdrop_feepayer_balance_after_tx() {
    init_logger!();

    // Build an Ephemeral validator config that enables auto airdrop for fee payers
    let config = ValidatorParams {
        lifecycle: LifecycleMode::Ephemeral,
        remotes: vec![
            Remote::from_str(IntegrationTestContext::url_chain()).unwrap(),
            Remote::from_str(IntegrationTestContext::ws_url_chain()).unwrap(),
        ],
        accountsdb: AccountsDbConfig::default(),
        chainlink: ChainLinkConfig {
            auto_airdrop_lamports: 1_000_000_000,
            ..Default::default()
        },
        ledger: LedgerConfig {
            reset: true,
            ..Default::default()
        },
        ..Default::default()
    };

    // Start the validator
    let (_tmpdir, Some(mut validator), port) =
        start_magicblock_validator_with_config_struct(
            config,
            &LoadedAccounts::with_delegation_program_test_authority(),
        )
    else {
        panic!("validator should set up correctly");
    };

    // Create context and wait for the ephem validator to start producing slots
    let ctx = expect!(
        IntegrationTestContext::try_new_with_ephem_port(port),
        validator
    );
    expect!(ctx.wait_for_next_slot_ephem(), validator);

    // Create a brand new fee payer with zero balance
    let payer = Keypair::new();
    let recipient = Keypair::new();

    // Send a zero lamport transfers to trigger account creation/cloning for the new fee payer
    // This should cause the validator to auto-airdrop 1 SOL to the payer
    // NOTE: we send two instructions to bypass the noop transaction optimization
    let ix1 =
        system_instruction::transfer(&payer.pubkey(), &recipient.pubkey(), 0);
    let ix2 =
        system_instruction::transfer(&payer.pubkey(), &recipient.pubkey(), 0);
    let (sig, confirmed) = expect!(
        ctx.send_and_confirm_instructions_with_payer_ephem(&[ix1, ix2], &payer),
        validator
    );

    assert!(
        !confirmed,
        cleanup(&mut validator),
        "Transaction is not confirmed (due to invalid writable): {sig}",
    );

    // Fetch the payer balance from the ephemeral validator and assert it equals 1_000_000_000
    let balance =
        expect!(ctx.fetch_ephem_account_balance(&payer.pubkey()), validator);
    assert_eq!(balance, 1_000_000_000, cleanup(&mut validator));

    // Cleanup validator process
    cleanup(&mut validator);
}
