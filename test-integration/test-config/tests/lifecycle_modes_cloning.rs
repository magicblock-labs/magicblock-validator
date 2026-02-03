use std::str::FromStr;

use cleanass::assert_eq;
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
use solana_sdk::pubkey::Pubkey;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
};
use test_kit::init_logger;
use tracing::*;

fn random_pubkey() -> Pubkey {
    Keypair::new().pubkey()
}

// Ephemeral mode: all accounts should be cloned
#[test]
#[file_serial]
fn test_lifecycle_ephemeral_clones_non_program_account() {
    run_lifecycle_cloning_test(
        LifecycleMode::Ephemeral,
        random_pubkey(),
        true,
        true,
    );
}

#[test]
#[file_serial]
fn test_lifecycle_ephemeral_clones_program_account() {
    run_lifecycle_cloning_test(
        LifecycleMode::Ephemeral,
        program_flexi_counter::id(),
        false,
        true,
    );
}

// Offline mode: no accounts should be cloned
#[test]
#[file_serial]
fn test_lifecycle_offline_does_not_clone_non_program_account() {
    run_lifecycle_cloning_test(
        LifecycleMode::Offline,
        random_pubkey(),
        true,
        false,
    );
}

#[test]
#[file_serial]
fn test_lifecycle_offline_does_not_clone_program_account() {
    run_lifecycle_cloning_test(
        LifecycleMode::Offline,
        program_flexi_counter::id(),
        false,
        false,
    );
}

// Replica mode: all accounts should be cloned
#[test]
#[file_serial]
fn test_lifecycle_replica_clones_non_program_account() {
    run_lifecycle_cloning_test(
        LifecycleMode::Replica,
        random_pubkey(),
        true,
        true,
    );
}

#[test]
#[file_serial]
fn test_lifecycle_replica_clones_program_account() {
    run_lifecycle_cloning_test(
        LifecycleMode::Replica,
        program_flexi_counter::id(),
        true,
        true,
    );
}

// Programs replica mode: only program accounts should be cloned
#[test]
#[file_serial]
fn test_lifecycle_programs_replica_does_not_clone_non_program_account() {
    run_lifecycle_cloning_test(
        LifecycleMode::ProgramsReplica,
        random_pubkey(),
        true,
        false,
    );
}

#[test]
#[file_serial]
fn test_lifecycle_programs_replica_clones_program_account() {
    run_lifecycle_cloning_test(
        LifecycleMode::ProgramsReplica,
        program_flexi_counter::id(),
        true,
        true,
    );
}

fn run_lifecycle_cloning_test(
    lifecycle_mode: LifecycleMode,
    pubkey: Pubkey,
    airdrop: bool,
    expect_clone: bool,
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

    if airdrop {
        // Airdrop to test account on chain
        expect!(ctx.airdrop_chain(&pubkey, LAMPORTS_PER_SOL), validator);
        debug!("✅ Airdropped 1 SOL to test account on chain: {}", pubkey);
    }

    // Attempt to fetch the account from ephemeral validator
    std::thread::sleep(std::time::Duration::from_millis(500));
    let cloned_account = ctx.fetch_ephem_account(pubkey);

    if expect_clone {
        assert_eq!(
            cloned_account.is_ok(),
            true,
            cleanup(&mut validator),
            "Account should have been cloned in {:?} mode for",
            lifecycle_mode,
        );
    } else {
        assert_eq!(
            cloned_account.is_err(),
            true,
            cleanup(&mut validator),
            "Account should NOT have been cloned in {:?} mode",
            lifecycle_mode,
        );
    }

    cleanup(&mut validator);
}
