use cleanass::assert_eq;
use std::{path::Path, process::Child};

use integration_test_tools::{expect, tmpdir::resolve_tmp_dir};
use program_flexi_counter::instruction::{
    create_add_and_schedule_commit_ix, create_add_counter_ix, create_add_ix,
    create_mul_ix,
};
use program_flexi_counter::{
    instruction::{create_delegate_ix, create_init_ix},
    state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
};
use test_ledger_restore::{
    assert_counter_commits_on_chain, cleanup, confirm_tx_with_payer_chain,
    confirm_tx_with_payer_ephem, fetch_counter_chain, fetch_counter_ephem,
    get_programs_with_flexi_counter, setup_validator_with_local_remote,
    wait_for_ledger_persist, TMP_DIR_LEDGER,
};
const COUNTER_MAIN: &str = "Main Counter";
const COUNTER_READONLY: &str = "Readonly Counter";
fn payer_keypair() -> Keypair {
    Keypair::new()
}

// In this test we work with two PDAs.
// - Main PDA owned by main payer, delegated to the ephemeral
// - Readonly PDA that is delegated
//
// We perform the following actions while the validator is running:
//
// 1. Init Main PDA and delegate it to the ephemeral and add 2
// 2. Init Readonly PDA and add 3
// 3. Run instruction that adds Readonly count to Main PDA count
//
// At this point the accounts have the following state in the ephemeral:
// Main PDA: count = 5, updates = 2
// Readonly PDA: count = 3, updates = 1
//
// Then we restart the validator and first verify that the accounts
// are in the same state.
// Finally we add 1 to the Readonly PDA on mainnet which should trigger
// a subscription update.
//
// We verify this by doing the following:
// 1. Run instruction that adds Readonly count to Main PDA count
// 2. Verify that the Main PDA count is 6 and updates = 3

#[test]
fn restore_ledger_using_readonlyestore_ledger_using_readonly() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let payer_main = payer_keypair();
    let payer_readonly = payer_keypair();

    let (mut validator, _) = write(&ledger_path, &payer_main, &payer_readonly);
    // validator.kill().unwrap();

    // let mut validator = read(&ledger_path, &payer_main, &payer_readonly);
    // validator.kill().unwrap();
}

fn write(
    ledger_path: &Path,
    payer_main: &Keypair,
    payer_readonly: &Keypair,
) -> (Child, u64) {
    let programs = get_programs_with_flexi_counter();

    let (_, mut validator, ctx) =
        setup_validator_with_local_remote(ledger_path, Some(programs), true);

    // Airdrop to payers on chain
    expect!(
        ctx.airdrop_chain(&payer_main.pubkey(), LAMPORTS_PER_SOL),
        validator
    );
    expect!(
        ctx.airdrop_chain(&payer_readonly.pubkey(), LAMPORTS_PER_SOL),
        validator
    );

    // Create and send init counter instructions on chain
    confirm_tx_with_payer_chain(
        create_init_ix(payer_main.pubkey(), COUNTER_MAIN.to_string()),
        payer_main,
        &mut validator,
    );
    confirm_tx_with_payer_chain(
        create_init_ix(payer_readonly.pubkey(), COUNTER_READONLY.to_string()),
        payer_readonly,
        &mut validator,
    );

    // Delegate main counter to ephemeral and add 2
    {
        confirm_tx_with_payer_chain(
            create_delegate_ix(payer_main.pubkey()),
            payer_main,
            &mut validator,
        );

        let ix = create_add_ix(payer_main.pubkey(), 2);
        confirm_tx_with_payer_ephem(ix, payer_main, &mut validator);

        let counter_main_ephem =
            fetch_counter_ephem(&payer_main.pubkey(), &mut validator);

        assert_eq!(
            counter_main_ephem,
            FlexiCounter {
                count: 2,
                updates: 1,
                label: COUNTER_MAIN.to_string()
            },
            cleanup(&mut validator)
        );
    }
    // Add 3 to Readonly Counter on chain
    {
        let ix = create_add_ix(payer_readonly.pubkey(), 3);
        confirm_tx_with_payer_chain(ix, payer_readonly, &mut validator);

        let counter_readonly_chain =
            fetch_counter_chain(&payer_readonly.pubkey(), &mut validator);
        assert_eq!(
            counter_readonly_chain,
            FlexiCounter {
                count: 3,
                updates: 1,
                label: COUNTER_READONLY.to_string()
            },
            cleanup(&mut validator)
        );
    }

    // Add Readonly Counter to Main Counter
    // At this point readonly counter is cloned into ephemeral
    {
        let ix =
            create_add_counter_ix(payer_main.pubkey(), payer_readonly.pubkey());
        confirm_tx_with_payer_ephem(ix, payer_main, &mut validator);

        let counter_main_ephem =
            fetch_counter_ephem(&payer_main.pubkey(), &mut validator);
        assert_eq!(
            counter_main_ephem,
            FlexiCounter {
                count: 5,
                updates: 2,
                label: COUNTER_MAIN.to_string()
            },
            cleanup(&mut validator)
        );
    }

    let slot = wait_for_ledger_persist(&mut validator);
    (validator, slot)
}

fn read(
    ledger_path: &Path,
    payer_main_kp: &Keypair,
    payer_readonly_kp: &Keypair,
) -> Child {
    let payer_main = &payer_main_kp.pubkey();
    let payer_readonly = &payer_readonly_kp.pubkey();
    let programs = get_programs_with_flexi_counter();

    let (_, mut validator, _) =
        setup_validator_with_local_remote(ledger_path, Some(programs), false);

    let counter_main_ephem = fetch_counter_ephem(payer_main, &mut validator);
    assert_eq!(
        counter_main_ephem,
        FlexiCounter {
            count: 5,
            updates: 2,
            label: COUNTER_MAIN.to_string()
        },
        cleanup(&mut validator)
    );

    let counter_readonly_ephem =
        fetch_counter_ephem(payer_readonly, &mut validator);
    assert_eq!(
        counter_readonly_ephem,
        FlexiCounter {
            count: 3,
            updates: 1,
            label: COUNTER_READONLY.to_string()
        },
        cleanup(&mut validator)
    );

    // Update readonly counter on chain and ensure it was cloned again
    {
        let ix = create_add_ix(*payer_readonly, 1);
        confirm_tx_with_payer_chain(ix, payer_readonly_kp, &mut validator);

        let counter_readonly_chain =
            fetch_counter_chain(payer_readonly, &mut validator);
        assert_eq!(
            counter_readonly_chain,
            FlexiCounter {
                count: 4,
                updates: 2,
                label: COUNTER_READONLY.to_string()
            },
            cleanup(&mut validator)
        );

        // NOTE: for now the ephem validator keeps the old state of the readonly account
        // since at this point we re-clone lazily. This will be fixed with the new
        // cloning pipeline at which point we need to update this test.
        let counter_readonly_ephem =
            fetch_counter_ephem(payer_readonly, &mut validator);
        assert_eq!(
            counter_readonly_ephem,
            FlexiCounter {
                count: 3,
                updates: 1,
                label: COUNTER_READONLY.to_string()
            },
            cleanup(&mut validator)
        );
    }

    // TODO: @@@ Then perform counter add again to ensure that the state is updated in the ephemeral
    // and this new state is used in the transaction

    validator
}
