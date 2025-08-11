use cleanass::assert_eq;
use integration_test_tools::loaded_accounts::LoadedAccounts;
use integration_test_tools::validator::cleanup;
use std::{path::Path, process::Child};

use integration_test_tools::{expect, tmpdir::resolve_tmp_dir};
use program_flexi_counter::instruction::{
    create_add_counter_ix, create_add_ix,
};
use program_flexi_counter::{
    instruction::{create_delegate_ix, create_init_ix},
    state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
};
use test_ledger_restore::{
    confirm_tx_with_payer_chain, confirm_tx_with_payer_ephem,
    fetch_counter_chain, fetch_counter_ephem, get_programs_with_flexi_counter,
    kill_validator, setup_validator_with_local_remote,
    wait_for_cloned_accounts_hydration, wait_for_ledger_persist,
    TMP_DIR_LEDGER,
};
const COUNTER_MAIN: &str = "Main Counter";
const COUNTER_READONLY: &str = "Readonly Counter";
fn payer_keypair() -> Keypair {
    Keypair::new()
}

// In this test we work with several accounts.
//
// - Wallet accounts
// - Main PDA owned by main payer, delegated to the ephemeral
// - Readonly PDA that is not delegated
//
// We restore the ledger multiple times to ensure that the first restore doesn't affect
// the second, i.e. we had a account_mod ID bug related to this.
//
// NOTE: this same setup is repeated in ./10_readonly_update_after.rs except
// we only check here that we can properly restore all of these accounts at all
#[test]
fn restore_ledger_different_accounts_multiple_times() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let payer_main = payer_keypair();
    let payer_readonly = payer_keypair();

    let (mut validator, _, payer_main_lamports) =
        write(&ledger_path, &payer_main, &payer_readonly);
    kill_validator(&mut validator, 8899);

    for _ in 0..5 {
        let mut validator = read(
            &ledger_path,
            &payer_main,
            &payer_readonly,
            payer_main_lamports,
        );
        kill_validator(&mut validator, 8899);
    }
}

fn write(
    ledger_path: &Path,
    payer_main: &Keypair,
    payer_readonly: &Keypair,
) -> (Child, u64, u64) {
    let programs = get_programs_with_flexi_counter();

    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        Some(programs),
        true,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

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

    let payer_main_ephem_lamports = expect!(
        ctx.fetch_ephem_account_balance(&payer_main.pubkey()),
        validator
    );

    let slot = wait_for_ledger_persist(&mut validator);
    (validator, slot, payer_main_ephem_lamports)
}

fn read(
    ledger_path: &Path,
    payer_main_kp: &Keypair,
    payer_readonly_kp: &Keypair,
    payer_main_lamports: u64,
) -> Child {
    let payer_main = &payer_main_kp.pubkey();
    let payer_readonly = &payer_readonly_kp.pubkey();
    let programs = get_programs_with_flexi_counter();

    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        Some(programs),
        false,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    wait_for_cloned_accounts_hydration();

    let payer_main_ephem =
        expect!(ctx.fetch_ephem_account_balance(payer_main), validator);
    assert_eq!(
        payer_main_ephem, payer_main_lamports,
        cleanup(&mut validator)
    );

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

    validator
}
