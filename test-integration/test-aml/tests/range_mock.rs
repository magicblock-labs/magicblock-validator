use std::{thread::sleep, time::Duration};

use cleanass::assert;
use ephemeral_rollups_sdk::spl::{
    builders::{
        InitializeGlobalVaultBuilder, InitializeRentPdaBuilder,
        SetupAndDelegateShuttleEphemeralAtaWithMergeBuilder,
    },
    find_rent_pda, find_shuttle_ata, find_shuttle_ephemeral_ata,
};
use integration_test_tools::{
    expect,
    loaded_accounts::{LoadedAccounts, DLP_TEST_AUTHORITY_BYTES},
    tmpdir::resolve_tmp_dir,
};
use magicblock_core::token_programs::derive_ata;
use solana_sdk::{
    program_pack::Pack, signature::Keypair, signer::Signer,
    transaction::Transaction,
};
use solana_system_interface::instruction as system_instruction;
use spl_associated_token_account_interface::instruction::create_associated_token_account_idempotent;
use spl_token::{instruction as spl_token_ix, state::Mint};
use test_aml::{
    cleanup_both, delegation_record_exists, delegation_record_persists,
    setup_validator_with_local_remote, wait_for_delegation_record_absent,
    MockRangeServer,
};
use test_kit::init_logger;

const SHUTTLE_AMOUNT: u64 = 200;
const SHUTTLE_ID: u32 = 0;
const TMP_DIR_LEDGER: &str = "TMP_DIR_LEDGER";

#[test]
fn test_risky_shuttle_owner_merge_is_blocked() {
    run_shuttle_merge_risk_case(9, false);
}

#[test]
fn test_low_risk_shuttle_owner_merge_is_allowed() {
    run_shuttle_merge_risk_case(1, true);
}

/// Drives the shuttle + merge flow for a single owner risk score and asserts
/// the AML risk gate decision.
///
/// `SetupAndDelegateShuttleEphemeralAtaWithMerge` delegates the shuttle ATA with
/// the merge attached as a post-delegation action. The merge's only signer is
/// the shuttle `owner`, so the validator risk-checks the owner before acting on
/// the delegation:
/// - a low-risk owner is allowed, so the shuttle ATA stays delegated;
/// - a risky owner is blocked, so the shuttle ATA is undelegated back to chain
///   instead of running the merge.
///
/// Both cases must query the Range risk service for the owner.
fn run_shuttle_merge_risk_case(owner_risk: u64, expect_allowed: bool) {
    init_logger!();

    let fee_payer = Keypair::new();
    let owner = Keypair::new();
    let recipient = Keypair::new();
    let mint = Keypair::new();
    let source_ata = derive_ata(&owner.pubkey(), &mint.pubkey());
    let destination_ata = derive_ata(&recipient.pubkey(), &mint.pubkey());
    let validator_pk = Keypair::try_from(&DLP_TEST_AUTHORITY_BYTES[..])
        .unwrap()
        .pubkey();

    let (shuttle_ephemeral_ata, _) =
        find_shuttle_ephemeral_ata(&owner.pubkey(), &mint.pubkey(), SHUTTLE_ID);
    let (shuttle_ata, _) =
        find_shuttle_ata(&shuttle_ephemeral_ata, &mint.pubkey());

    let mut server = MockRangeServer::start().unwrap();
    server.set_risk(&owner.pubkey().to_string(), owner_risk);

    let (_tmpdir, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        &ledger_path,
        None,
        true,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
        server.base_url().to_string(),
    );

    expect!(
        ctx.airdrop_chain(&fee_payer.pubkey(), 2_000_000_000),
        validator
    );
    expect!(ctx.airdrop_chain(&owner.pubkey(), 2_000_000_000), validator);

    let chain_client = expect!(ctx.try_chain_client(), validator);
    let mint_rent = expect!(
        chain_client.get_minimum_balance_for_rent_exemption(Mint::LEN),
        validator
    );

    // Create the mint, the owner's funded source ATA and the recipient's
    // destination ATA the merge targets.
    let setup_ixs = vec![
        system_instruction::create_account(
            &fee_payer.pubkey(),
            &mint.pubkey(),
            mint_rent,
            Mint::LEN as u64,
            &spl_token::id(),
        ),
        expect!(
            spl_token_ix::initialize_mint(
                &spl_token::id(),
                &mint.pubkey(),
                &owner.pubkey(),
                None,
                0,
            ),
            validator
        ),
        create_associated_token_account_idempotent(
            &fee_payer.pubkey(),
            &owner.pubkey(),
            &mint.pubkey(),
            &spl_token::id(),
        ),
        create_associated_token_account_idempotent(
            &fee_payer.pubkey(),
            &recipient.pubkey(),
            &mint.pubkey(),
            &spl_token::id(),
        ),
        expect!(
            spl_token_ix::mint_to(
                &spl_token::id(),
                &mint.pubkey(),
                &source_ata,
                &owner.pubkey(),
                &[],
                SHUTTLE_AMOUNT,
            ),
            validator
        ),
    ];
    let mut setup_tx =
        Transaction::new_with_payer(&setup_ixs, Some(&fee_payer.pubkey()));
    let (_sig, confirmed) = expect!(
        ctx.send_and_confirm_transaction_chain(
            &mut setup_tx,
            &[&fee_payer, &mint, &owner],
        ),
        validator
    );
    assert!(
        confirmed,
        cleanup_both(&mut validator, &mut server),
        "mint/ATA setup transaction failed"
    );

    // Initialize the shuttle prerequisites: the rent PDA (a shared vault that
    // fronts rent for the delegated shuttle accounts, so it must hold lamports
    // beyond its own rent exemption) and the per-mint global vault.
    // The rent PDA is global and shared across tests, so only initialize it
    // when it does not exist yet.
    let (rent_pda, _) = find_rent_pda();
    if ctx.fetch_chain_account(rent_pda).is_err() {
        let mut prereq_tx = Transaction::new_with_payer(
            &[InitializeRentPdaBuilder {
                payer: fee_payer.pubkey(),
            }
            .instruction()],
            Some(&fee_payer.pubkey()),
        );
        let (_sig, confirmed) = expect!(
            ctx.send_and_confirm_transaction_chain(
                &mut prereq_tx,
                &[&fee_payer]
            ),
            validator
        );
        assert!(
            confirmed,
            cleanup_both(&mut validator, &mut server),
            "rent PDA initialization failed"
        );
    }
    expect!(ctx.airdrop_chain(&rent_pda, 1_000_000_000), validator);

    let mut vault_tx = Transaction::new_with_payer(
        &[InitializeGlobalVaultBuilder {
            payer: fee_payer.pubkey(),
            mint: mint.pubkey(),
        }
        .instruction()],
        Some(&fee_payer.pubkey()),
    );
    let (_sig, confirmed) = expect!(
        ctx.send_and_confirm_transaction_chain(&mut vault_tx, &[&fee_payer]),
        validator
    );
    assert!(
        confirmed,
        cleanup_both(&mut validator, &mut server),
        "global vault initialization failed"
    );

    // Delegate the shuttle ATA with the merge attached as a post-delegation
    // action.
    let mut shuttle_tx = Transaction::new_with_payer(
        &[SetupAndDelegateShuttleEphemeralAtaWithMergeBuilder {
            payer: fee_payer.pubkey(),
            owner: owner.pubkey(),
            mint: mint.pubkey(),
            source_ata,
            destination_ata,
            shuttle_id: SHUTTLE_ID,
            amount: SHUTTLE_AMOUNT,
            validator: Some(validator_pk),
        }
        .instruction()],
        Some(&fee_payer.pubkey()),
    );
    let (_sig, confirmed) = expect!(
        ctx.send_and_confirm_transaction_chain(
            &mut shuttle_tx,
            &[&fee_payer, &owner],
        ),
        validator
    );
    assert!(
        confirmed,
        cleanup_both(&mut validator, &mut server),
        "shuttle setup + delegation transaction failed"
    );
    assert!(
        delegation_record_exists(&ctx, &shuttle_ata),
        cleanup_both(&mut validator, &mut server),
        "shuttle ATA delegation record was not created on base chain"
    );

    // The validator clones the delegated shuttle ATA and risk-checks the owner
    // (the merge's only signer) before acting on the delegation.
    let mut risk_checked = false;
    for _ in 0..60 {
        if server.request_count() > 0 {
            risk_checked = true;
            break;
        }
        sleep(Duration::from_millis(200));
    }
    assert!(
        risk_checked,
        cleanup_both(&mut validator, &mut server),
        "Range risk server was not queried"
    );
    let requested_addresses = server.requested_addresses();
    assert!(
        requested_addresses.contains(&owner.pubkey().to_string()),
        cleanup_both(&mut validator, &mut server),
        "Range risk server did not check shuttle owner; requested: {:?}",
        requested_addresses
    );

    if expect_allowed {
        // Low-risk owner: the merge is allowed, so the shuttle ATA keeps its
        // delegation and is never undelegated back to chain.
        assert!(
            delegation_record_persists(&ctx, &shuttle_ata),
            cleanup_both(&mut validator, &mut server),
            "low-risk shuttle ATA was unexpectedly undelegated on base chain"
        );
    } else {
        // Risky owner: the merge is blocked, so the shuttle ATA is undelegated
        // back to the base chain instead.
        assert!(
            wait_for_delegation_record_absent(&ctx, &shuttle_ata),
            cleanup_both(&mut validator, &mut server),
            "high-risk shuttle ATA was not undelegated on base chain"
        );
    }

    cleanup_both(&mut validator, &mut server);
}
