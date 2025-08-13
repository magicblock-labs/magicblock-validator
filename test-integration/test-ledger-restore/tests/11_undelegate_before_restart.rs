use cleanass::assert;
use integration_test_tools::conversions::get_rpc_transwise_error_msg;
use integration_test_tools::loaded_accounts::LoadedAccounts;
use integration_test_tools::validator::cleanup;
use integration_test_tools::{expect, tmpdir::resolve_tmp_dir};
use integration_test_tools::{expect_err, unwrap, IntegrationTestContext};
use program_flexi_counter::instruction::{
    create_add_and_schedule_commit_ix, create_add_ix,
};
use program_flexi_counter::instruction::{create_delegate_ix, create_init_ix};
use program_flexi_counter::state::FlexiCounter;
use solana_sdk::transaction::Transaction;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
};
use std::path::Path;
use std::process::Child;
use test_ledger_restore::{
    assert_counter_state, confirm_tx_with_payer_chain,
    confirm_tx_with_payer_ephem, get_programs_with_flexi_counter,
    kill_validator, setup_validator_with_local_remote,
    wait_for_cloned_accounts_hydration, wait_for_ledger_persist, Counter,
    State, TMP_DIR_LEDGER,
};

const COUNTER: &str = "Counter of Payer";
fn payer_keypair() -> Keypair {
    Keypair::new()
}

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

#[test]
fn restore_ledger_with_account_undelegated_before_restart() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let payer = payer_keypair();

    // Original instance delegates and updates account
    let (mut validator, _) = write(&ledger_path, &payer);
    kill_validator(&mut validator, 8899);

    // Undelegate account while validator is down (note we do this by starting
    // another instance, to use the same validator auth)
    let mut validator = update_counter_between_restarts(&payer);
    kill_validator(&mut validator, 8899);

    // Now we restart the validator pointing at the original ledger path
    let mut validator = read(&ledger_path, &payer);
    kill_validator(&mut validator, 8899);
}

fn write(ledger_path: &Path, payer: &Keypair) -> (Child, u64) {
    let programs = get_programs_with_flexi_counter();

    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        Some(programs),
        true,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    // Airdrop to payer on chain
    expect!(
        ctx.airdrop_chain(&payer.pubkey(), LAMPORTS_PER_SOL),
        validator
    );

    // Create and send init counter instruction on chain
    confirm_tx_with_payer_chain(
        create_init_ix(payer.pubkey(), COUNTER.to_string()),
        payer,
        &mut validator,
    );

    // Delegate counter to ephemeral
    confirm_tx_with_payer_chain(
        create_delegate_ix(payer.pubkey()),
        payer,
        &mut validator,
    );

    // Add 2 to counter in ephemeral
    let ix = create_add_ix(payer.pubkey(), 2);
    confirm_tx_with_payer_ephem(ix, payer, &mut validator);

    assert_counter_state!(
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

    let slot = wait_for_ledger_persist(&mut validator);
    (validator, slot)
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

    let ix = create_add_and_schedule_commit_ix(payer.pubkey(), 3, true);
    let sig = confirm_tx_with_payer_ephem(ix, payer, &mut validator);
    let res = expect!(
        ctx.fetch_schedule_commit_result::<FlexiCounter>(sig),
        validator
    );
    expect!(res.confirm_commit_transactions_on_chain(&ctx), validator);

    // NOTE: that the account was never committed before the previous
    // validator instance shut down, thus we start from 0:0 again when
    // we add 3
    assert_counter_state!(
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

    validator
}

fn read(ledger_path: &Path, payer: &Keypair) -> Child {
    let programs = get_programs_with_flexi_counter();

    let (_, mut validator, _) = setup_validator_with_local_remote(
        ledger_path,
        Some(programs),
        false,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    let ix = create_add_ix(payer.pubkey(), 1);
    let ctx = expect!(IntegrationTestContext::try_new_ephem_only(), validator);

    wait_for_cloned_accounts_hydration();

    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    let signers = &[payer];

    let err = expect_err!(
        ctx.send_and_confirm_transaction_ephem(&mut tx, signers),
        validator
    );
    let tx_err = unwrap!(get_rpc_transwise_error_msg(&err), validator);
    assert!(
        tx_err.contains("TransactionIncludeUndelegatedAccountsAsWritable"),
        cleanup(&mut validator)
    );

    validator
}
