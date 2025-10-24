use std::{path::Path, process::Child};

use cleanass::assert;
use integration_test_tools::{
    conversions::get_rpc_transwise_error_msg, expect, expect_err,
    loaded_accounts::LoadedAccounts, tmpdir::resolve_tmp_dir, unwrap,
    validator::cleanup,
};
use log::*;
use program_flexi_counter::{
    instruction::{
        create_add_and_schedule_commit_ix, create_add_ix, create_delegate_ix,
        create_init_ix,
    },
    state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
    transaction::Transaction,
};
use test_kit::init_logger;
use test_ledger_restore::{
    airdrop_accounts_on_chain, assert_counter_state,
    confirm_tx_with_payer_chain, confirm_tx_with_payer_ephem,
    delegate_accounts, get_programs_with_flexi_counter,
    setup_validator_with_local_remote, wait_for_ledger_persist, Counter, State,
    TMP_DIR_LEDGER,
};

const COUNTER: &str = "Counter of Payer";

// In this test we init and then delegate an account.
// Then we add to it and shut down the validator
//
// While the validator is shut down we undelegate the account on chain and then
// add to it again (on mainnet).
//
// Then we restart the validator and do the following:
//
// 1. Check that it was cloned with the updated state
// 2. Verify that it is no longer useable as as delegated account in the validator

// Tracking: https://github.com/magicblock-labs/magicblock-validator/issues/565
#[ignore = "This is currently no longer supported since we don't hydrate delegated accounts on startup"]
#[test]
fn test_restore_ledger_with_account_undelegated_before_restart() {
    init_logger!();
    let (_tmpdir, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    // Original instance delegates and updates account
    let (mut validator, _, payer) = write(&ledger_path);
    validator.kill().unwrap();

    // Undelegate account while validator is down (note we do this by starting
    // another instance, to use the same validator auth)
    let mut validator = update_counter_between_restarts(&payer);
    validator.kill().unwrap();

    // Now we restart the validator pointing at the original ledger path
    let mut validator = read(&ledger_path, &payer);
    validator.kill().unwrap();
}

fn write(ledger_path: &Path) -> (Child, u64, Keypair) {
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        true,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    // Airdrop to payer on chain
    let mut keypairs = airdrop_accounts_on_chain(
        &ctx,
        &mut validator,
        &[2 * LAMPORTS_PER_SOL],
    );
    let payer = keypairs.drain(0..1).next().unwrap();

    debug!("✅ Airdropped to payer {} on chain", payer.pubkey());

    // Create and send init counter instruction on chain
    let (counter_pda, _) = FlexiCounter::pda(&payer.pubkey());
    confirm_tx_with_payer_chain(
        create_init_ix(payer.pubkey(), COUNTER.to_string()),
        &payer,
        &mut validator,
    );
    debug!(
        "✅ Initialized counter {counter_pda} for payer {} on chain",
        payer.pubkey()
    );

    // Delegate counter to ephemeral
    confirm_tx_with_payer_chain(
        create_delegate_ix(payer.pubkey()),
        &payer,
        &mut validator,
    );
    debug!("✅ Delegated counter {counter_pda} on chain");

    // Delegate payer so we can use it in ephemeral
    delegate_accounts(&ctx, &mut validator, &[&payer]);
    debug!("✅ Delegated payer {} to ephemeral", payer.pubkey());

    // Add 2 to counter in ephemeral
    let ix = create_add_ix(payer.pubkey(), 2);
    confirm_tx_with_payer_ephem(ix, &payer, &ctx, &mut validator);
    debug!("✅ Added 2 to counter {counter_pda} in ephemeral");

    assert_counter_state!(
        &ctx,
        &mut validator,
        Counter {
            payer: &payer.pubkey(),
            chain: State {
                count: 0,
                updates: 0,
            },
            ephem: State {
                count: 2,
                updates: 1,
            },
        },
        COUNTER
    );
    debug!("✅ Verified counter state after adding 2");

    let slot = wait_for_ledger_persist(&ctx, &mut validator);
    debug!("✅ Ledger persisted at slot {slot}");
    (validator, slot, payer)
}

fn update_counter_between_restarts(payer: &Keypair) -> Child {
    // We start another validator instance pointing at a separate ledger path
    // Then we fund the same payer again and finally update the counter
    // adding 3 and undelegating the account on chain
    // before restarting the validator
    let (_, ledger_path) =
        resolve_tmp_dir("FORCE_UNIQUE_TMP_DIR_AND_IGNORE_THIS_ENV_VAR");
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        &ledger_path,
        None,
        true,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    let (counter_pda, _) = FlexiCounter::pda(&payer.pubkey());

    // Delegate payer so we can use it in ephemeral
    //     delegate_accounts(&ctx, &mut validator, &[payer]);
    //     debug!(
    //         "✅ Delegated payer {} in new validator instance",
    //         payer.pubkey()
    //     );

    let ix = create_add_and_schedule_commit_ix(payer.pubkey(), 3, true);
    let sig = confirm_tx_with_payer_ephem(ix, payer, &ctx, &mut validator);
    debug!("✅ Added 3 and scheduled commit to counter {counter_pda} with undelegation");

    let res = expect!(
        ctx.fetch_schedule_commit_result::<FlexiCounter>(sig),
        validator
    );
    expect!(res.confirm_commit_transactions_on_chain(&ctx), validator);
    debug!("✅ Confirmed commit transactions on chain (undelegate=true)");

    // NOTE: that the account was never committed before the previous
    // validator instance shut down, thus we start from 0:0 again when
    // we add 3
    assert_counter_state!(
        &ctx,
        &mut validator,
        Counter {
            payer: &payer.pubkey(),
            chain: State {
                count: 3,
                updates: 1,
            },
            ephem: State {
                count: 3,
                updates: 1,
            },
        },
        COUNTER
    );
    debug!("✅ Verified counter state after commit and undelegation");

    validator
}

fn read(ledger_path: &Path, payer: &Keypair) -> Child {
    let programs = get_programs_with_flexi_counter();

    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        Some(programs),
        false,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );
    debug!("✅ Started validator after restore");

    let ix = create_add_ix(payer.pubkey(), 1);

    let (counter_pda, _) = FlexiCounter::pda(&payer.pubkey());
    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    let signers = &[payer];

    // TODO(thlorenz): the below fails the following reason:
    // 1. the undelegation did go through when we started the validator pointing at different
    //    ledger
    // 2. the validator started from original ledger does not hydrate the delegated account and
    //    thus does not know it was undelegated in between restarts
    let res = ctx.send_and_confirm_transaction_ephem(&mut tx, signers);
    debug!("✅ Sent transaction to add 1 to counter {counter_pda} after restore: {res:#?}");

    let err = expect_err!(
        ctx.send_and_confirm_transaction_ephem(&mut tx, signers),
        validator
    );
    debug!("✅ Received expected error when trying to use undelegated account");

    let tx_err = unwrap!(get_rpc_transwise_error_msg(&err), validator);
    assert!(
        tx_err.contains("TransactionIncludeUndelegatedAccountsAsWritable"),
        cleanup(&mut validator)
    );
    debug!("✅ Verified error is TransactionIncludeUndelegatedAccountsAsWritable for counter {counter_pda}");

    validator
}
