use integration_test_tools::{
    expect, loaded_accounts::LoadedAccounts,
    validator::start_magicblock_validator_with_config_struct,
    IntegrationTestContext,
};
use magicblock_config::{
    AccountsCloneConfig, AccountsConfig, EphemeralConfig, LedgerConfig,
    LedgerResumeStrategyConfig, LedgerResumeStrategyType, LifecycleMode,
    RemoteCluster, RemoteConfig,
};
use solana_sdk::{signature::Keypair, signer::Signer, system_instruction};
use test_kit::init_logger;

#[ignore = "Auto airdrop is not generally supported at this point, we will add this back as needed"]
#[test]
fn test_auto_airdrop_feepayer_balance_after_tx() {
    init_logger!();

    // Build an Ephemeral validator config that enables auto airdrop for fee payers
    let config = EphemeralConfig {
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
                auto_airdrop_lamports: 1_000_000_000,
                ..Default::default()
            },
            ..Default::default()
        },
        ledger: LedgerConfig {
            resume_strategy_config: LedgerResumeStrategyConfig {
                kind: LedgerResumeStrategyType::Reset,
                ..Default::default()
            },
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

    // Create a brand new fee payer with zero balance on chain
    let payer = Keypair::new();
    let recipient = Keypair::new();

    // Send a 0-lamport transfer to trigger account creation/cloning for the new fee payer
    // This should cause the validator to auto-airdrop 1 SOL to the payer
    let ix =
        system_instruction::transfer(&payer.pubkey(), &recipient.pubkey(), 0);
    let _sig = expect!(
        ctx.send_and_confirm_instructions_with_payer_ephem(&[ix], &payer),
        validator
    );

    // Fetch the payer balance from the ephemeral validator and assert it equals 1_000_000_000
    let balance =
        expect!(ctx.fetch_ephem_account_balance(&payer.pubkey()), validator);
    assert_eq!(balance, 1_000_000_000);

    // Cleanup validator process
    integration_test_tools::validator::cleanup(&mut validator);
}
