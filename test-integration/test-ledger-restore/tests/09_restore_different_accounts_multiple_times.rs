use log::*;
use std::{path::Path, process::Child};
use test_kit::init_logger;

use cleanass::assert_eq;
use integration_test_tools::{
    expect, loaded_accounts::LoadedAccounts, tmpdir::resolve_tmp_dir,
    validator::cleanup,
};
use program_flexi_counter::{
    instruction::{create_add_counter_ix, create_add_ix, create_init_ix},
    state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
};
use test_ledger_restore::{
    confirm_tx_with_payer_chain, confirm_tx_with_payer_ephem,
    fetch_counter_chain, fetch_counter_ephem,
    init_and_delegate_counter_and_payer, setup_validator_with_local_remote,
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
fn test_restore_ledger_different_accounts_multiple_times() {
    init_logger!();
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let payer_readonly = payer_keypair();

    let (mut validator, _, payer_main_lamports, payer_main) =
        write(&ledger_path, &payer_readonly);
    validator.kill().unwrap();

    for _ in 0..5 {
        let mut validator = read(
            &ledger_path,
            &payer_main,
            &payer_readonly,
            payer_main_lamports,
        );
        validator.kill().unwrap();
    }
}

fn write(
    ledger_path: &Path,
    payer_readonly: &Keypair,
) -> (Child, u64, u64, Keypair) {
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        true,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    // Setup readonly counter
    {
        expect!(
            ctx.airdrop_chain(&payer_readonly.pubkey(), LAMPORTS_PER_SOL),
            validator
        );

        confirm_tx_with_payer_chain(
            create_init_ix(
                payer_readonly.pubkey(),
                COUNTER_READONLY.to_string(),
            ),
            payer_readonly,
            &mut validator,
        );
        let (counter_pda, _) = FlexiCounter::pda(&payer_readonly.pubkey());
        debug!(
            "✅ Initialized readonly counter {counter_pda} for payer {} on chain",
            payer_readonly.pubkey()
        );
    }

    // Setup main counter
    let (payer_main, counter_main) =
        init_and_delegate_counter_and_payer(&ctx, &mut validator, COUNTER_MAIN);

    debug!(
        "✅ Initialized and delegated main counter {counter_main} for payer {}",
        payer_main.pubkey()
    );

    // Add 2 to main counter in ephemeral
    {
        let ix = create_add_ix(payer_main.pubkey(), 2);
        confirm_tx_with_payer_ephem(ix, &payer_main, &ctx, &mut validator);

        let counter_main_ephem =
            fetch_counter_ephem(&ctx, &payer_main.pubkey(), &mut validator);

        assert_eq!(
            counter_main_ephem,
            FlexiCounter {
                count: 2,
                updates: 1,
                label: COUNTER_MAIN.to_string()
            },
            cleanup(&mut validator)
        );
        debug!("✅ Added 2 to Main Counter in ephemeral");
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
        debug!("✅ Added 3 to Readonly Counter on chain");
    }

    // Add Readonly Counter to Main Counter
    // At this point readonly counter is cloned into ephemeral
    {
        let ix =
            create_add_counter_ix(payer_main.pubkey(), payer_readonly.pubkey());
        confirm_tx_with_payer_ephem(ix, &payer_main, &ctx, &mut validator);

        let counter_main_ephem =
            fetch_counter_ephem(&ctx, &payer_main.pubkey(), &mut validator);
        assert_eq!(
            counter_main_ephem,
            FlexiCounter {
                count: 5,
                updates: 2,
                label: COUNTER_MAIN.to_string()
            },
            cleanup(&mut validator)
        );
        debug!("✅ Added Readonly Counter to Main Counter in ephemeral");
    }

    let payer_main_ephem_lamports = expect!(
        ctx.fetch_ephem_account_balance(&payer_main.pubkey()),
        validator
    );
    debug!("Payer main ephemeral lamports: {payer_main_ephem_lamports}");

    let slot = wait_for_ledger_persist(&ctx, &mut validator);
    (validator, slot, payer_main_ephem_lamports, payer_main)
}

fn read(
    ledger_path: &Path,
    payer_main_kp: &Keypair,
    payer_readonly_kp: &Keypair,
    payer_main_lamports: u64,
) -> Child {
    let payer_main = &payer_main_kp.pubkey();
    let payer_readonly = &payer_readonly_kp.pubkey();

    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
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
    debug!("✅ Verified main payer ephemeral lamports");

    let counter_readonly_ephem =
        fetch_counter_ephem(&ctx, payer_readonly, &mut validator);
    assert_eq!(
        counter_readonly_ephem,
        FlexiCounter {
            count: 3,
            updates: 1,
            label: COUNTER_READONLY.to_string()
        },
        cleanup(&mut validator)
    );
    debug!("✅ Verified readonly counter state after restore");

    let counter_main_ephem =
        fetch_counter_ephem(&ctx, payer_main, &mut validator);
    assert_eq!(
        counter_main_ephem,
        FlexiCounter {
            count: 5,
            updates: 2,
            label: COUNTER_MAIN.to_string()
        },
        cleanup(&mut validator)
    );
    debug!("✅ Verified main counter state after restore");

    validator
}
