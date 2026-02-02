use std::str::FromStr;

use cleanass::assert_eq;
use integration_test_tools::{
    expect,
    loaded_accounts::LoadedAccounts,
    validator::{cleanup, start_magicblock_validator_with_config_struct},
    IntegrationTestContext,
};
use tracing::*;
use magicblock_config::{
    config::{
        accounts::AccountsDbConfig, chain::ChainLinkConfig,
        ledger::LedgerConfig, LifecycleMode,
    },
    types::network::Remote,
    ValidatorParams,
};
use serial_test::file_serial;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
    system_instruction,
};
use test_kit::init_logger;

#[test]
#[file_serial]
fn test_lifecycle_ephemeral_blocks_system_transfer_to_non_delegated() {
    run_lifecycle_transfer_test(LifecycleMode::Ephemeral, false);
}

#[test]
#[file_serial]
fn test_lifecycle_offline_allows_system_transfer_to_non_delegated() {
    run_lifecycle_transfer_test(LifecycleMode::Offline, true);
}

#[test]
#[file_serial]
fn test_lifecycle_replica_allows_system_transfer_to_non_delegated() {
    run_lifecycle_transfer_test(LifecycleMode::Replica, true);
}

#[test]
#[file_serial]
fn test_lifecycle_programs_replica_allows_system_transfer_to_non_delegated() {
    run_lifecycle_transfer_test(LifecycleMode::ProgramsReplica, true);
}

fn run_lifecycle_transfer_test(
    lifecycle_mode: LifecycleMode,
    expect_success: bool,
) {
    init_logger!();

    let config = ValidatorParams {
        lifecycle: lifecycle_mode.clone(),
        remotes: vec![
            Remote::from_str(IntegrationTestContext::url_chain()).unwrap(),
            Remote::from_str(IntegrationTestContext::ws_url_chain()).unwrap(),
        ],
        chainlink: ChainLinkConfig {
            auto_airdrop_lamports: 0,
            ..Default::default()
        },
        accountsdb: AccountsDbConfig {
            reset: true,
            ..Default::default()
        },
        ledger: LedgerConfig {
            reset: true,
            ..Default::default()
        },
        ..Default::default()
    };

    let (_default_tmpdir, Some(mut validator), port) =
        start_magicblock_validator_with_config_struct(
            config,
            &LoadedAccounts::with_delegation_program_test_authority(),
        )
    else {
        panic!("validator should set up correctly");
    };

    let ctx = expect!(
        IntegrationTestContext::try_new_with_ephem_port(port),
        validator
    );

    // Create payer accounts
    let payer_chain = Keypair::new();
    let payer = Keypair::new();
    let non_delegated_recipient = Keypair::new();

    // Setup payer based on lifecycle mode
    match lifecycle_mode {
        LifecycleMode::Ephemeral => {
            expect!(
                ctx.airdrop_chain(&payer_chain.pubkey(), LAMPORTS_PER_SOL),
                validator
            );
            expect!(
                ctx.airdrop_chain_and_delegate(
                    &payer_chain,
                    &payer,
                    LAMPORTS_PER_SOL,
                ),
                validator
            );
            debug!(
                "✅ Airdropped 1 SOL to payer account via chain: {}",
                payer.pubkey()
            );
        }
        // All other modes support direct airdrop
        _ => {
            expect!(
                ctx.airdrop_ephem(&payer.pubkey(), LAMPORTS_PER_SOL),
                validator
            );
            debug!(
                "✅ Airdropped 1 SOL to payer account directly in offline \
                 validator: {}",
                payer.pubkey()
            );
        }
    }

    // 4. Send a transfer to a non-delegated account that doesn't yet exist
    // This tests the lifecycle mode behavior
    let ix = system_instruction::transfer(
        &payer.pubkey(),
        &non_delegated_recipient.pubkey(),
        1_000_000,
    );

    let (sig, confirmed) = expect!(
        ctx.send_and_confirm_instructions_with_payer_ephem(&[ix], &payer),
        validator
    );

    debug!(
        "Transfer to non-delegated {sig} recipient result confirmed: {}",
        confirmed
    );

    assert_eq!(
        confirmed,
        expect_success,
        cleanup(&mut validator),
        "Transfer to non-delegated account expected success: {}, got: {}",
        expect_success,
        confirmed
    );

    cleanup(&mut validator);
}
