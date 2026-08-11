use std::{thread::sleep, time::Duration};

use cleanass::assert;
use ephemeral_rollups_sdk::spl::{
    builders::{
        DelegateEphemeralAtaBuilder, InitializeEphemeralAtaBuilder,
        InitializeGlobalVaultBuilder, InitializeRentPdaBuilder,
        SetupAndDelegateShuttleEphemeralAtaWithMergeBuilder,
    },
    find_rent_pda, find_shuttle_ata, find_shuttle_ephemeral_ata,
};
use integration_test_tools::{
    expect, init_logger,
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
    cleanup_both, setup_validator_with_local_remote, token_balance_ephem,
    wait_for_delegation_record_absent, wait_for_delegation_record_present,
    wait_for_token_balance_ephem, MockRangeServer,
};

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
/// `SetupAndDelegateShuttleEphemeralAtaWithMerge` delegates the shuttle ATA and
/// attaches two post-delegation actions: the merge, then an undelegate-and-close
/// of the shuttle. The merge's only signer is the shuttle `owner`, so the
/// validator risk-checks the owner before acting on the delegation:
/// - a low-risk owner is allowed, so the merge runs and credits the recipient;
/// - a risky owner is blocked, so nothing runs and the recipient stays uncredited.
///
/// The shuttle ATA ends up undelegated either way — the allowed path gets there
/// through its own undelegate-and-close action, the blocked path because the
/// validator undelegates instead of running anything — so the token movement,
/// not the delegation record, is what separates the two verdicts.
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

    // The merge credits the recipient's ATA from inside the ephemeral rollup,
    // so that ATA has to be writable there. A plain SPL ATA is cloned read-only
    // and the merge would be rejected. Giving the recipient an ephemeral ATA
    // delegated to this validator is what lets chainlink project the eATA onto
    // the base ATA and clone it delegated, which is what makes it writable.
    let mut eata_tx = Transaction::new_with_payer(
        &[
            InitializeEphemeralAtaBuilder {
                payer: fee_payer.pubkey(),
                user: recipient.pubkey(),
                mint: mint.pubkey(),
            }
            .instruction(),
            DelegateEphemeralAtaBuilder {
                payer: fee_payer.pubkey(),
                user: recipient.pubkey(),
                mint: mint.pubkey(),
                validator: Some(validator_pk),
            }
            .instruction(),
        ],
        Some(&fee_payer.pubkey()),
    );
    let (_sig, confirmed) = expect!(
        ctx.send_and_confirm_transaction_chain(&mut eata_tx, &[&fee_payer]),
        validator
    );
    assert!(
        confirmed,
        cleanup_both(&mut validator, &mut server),
        "recipient ephemeral ATA setup failed"
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
    let record_created = wait_for_delegation_record_present(&ctx, &shuttle_ata);
    assert!(
        record_created,
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

    // Both verdicts end with the shuttle ATA undelegated: the allowed path runs
    // the merge and then the attached undelegate-and-close action, the blocked
    // path undelegates instead of running anything. What separates them is
    // whether the tokens actually moved.
    let was_undelegated = wait_for_delegation_record_absent(&ctx, &shuttle_ata);
    assert!(
        was_undelegated,
        cleanup_both(&mut validator, &mut server),
        "shuttle ATA was not undelegated on base chain"
    );

    // The merge credits the recipient inside the rollup, against the ATA
    // chainlink projects from the recipient's delegated eATA. The base chain
    // only sees those tokens once the eATA is settled, which this flow does not
    // do, so the rollup is where the verdict is observable.
    if expect_allowed {
        // Low-risk owner: the merge ran, so the shuttle's tokens landed in the
        // recipient's ATA.
        let merged = wait_for_token_balance_ephem(
            &ctx,
            &destination_ata,
            SHUTTLE_AMOUNT,
        );
        assert!(
            merged,
            cleanup_both(&mut validator, &mut server),
            "low-risk merge did not credit the recipient; balance: {:?}",
            token_balance_ephem(&ctx, &destination_ata)
        );
    } else {
        // Risky owner: the merge was blocked, so the recipient was never
        // credited.
        let balance = token_balance_ephem(&ctx, &destination_ata).unwrap_or(0);
        assert!(
            balance == 0,
            cleanup_both(&mut validator, &mut server),
            "high-risk merge credited the recipient with {balance}"
        );
    }

    cleanup_both(&mut validator, &mut server);
}
