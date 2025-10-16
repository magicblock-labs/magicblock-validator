use std::{path::Path, process::Child};

use cleanass::assert_eq;
use integration_test_tools::{
    expect, loaded_accounts::LoadedAccounts, tmpdir::resolve_tmp_dir,
    validator::cleanup,
};
use log::*;
use program_flexi_counter::{
    instruction::{
        create_add_and_schedule_commit_ix, create_add_ix, create_mul_ix,
    },
    state::FlexiCounter,
};
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};
use test_kit::init_logger;
use test_ledger_restore::{
    assert_counter_commits_on_chain, confirm_tx_with_payer_ephem,
    fetch_counter_chain, fetch_counter_ephem, get_programs_with_flexi_counter,
    init_and_delegate_counter_and_payer, setup_validator_with_local_remote,
    wait_for_cloned_accounts_hydration, wait_for_ledger_persist,
    TMP_DIR_LEDGER,
};

const COUNTER: &str = "Counter of Payer";

// In this test we update a delegated account in the ephemeral and then commit it.
// We then restore the ledger and verify that the committed account is available
// and that the commit was not run during ledger processing.

#[test]
fn test_restore_ledger_containing_delegated_and_committed_account() {
    init_logger!();
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (mut validator, _, payer) = write(&ledger_path);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &payer.pubkey());
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

    let (payer, counter) =
        init_and_delegate_counter_and_payer(&ctx, &mut validator, COUNTER);
    debug!(
        "✅ Initialized and delegated counter {counter} to payer {}",
        payer.pubkey()
    );

    {
        // Increment counter in ephemeral
        let ix = create_add_ix(payer.pubkey(), 3);
        confirm_tx_with_payer_ephem(ix, &payer, &ctx, &mut validator);
        let counter =
            fetch_counter_ephem(&ctx, &payer.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 3,
                updates: 1,
                label: COUNTER.to_string()
            },
            cleanup(&mut validator)
        );
        debug!("✅ Incremented counter in ephemeral");
    }

    {
        // Multiply counter in ephemeral
        let ix = create_mul_ix(payer.pubkey(), 2);
        confirm_tx_with_payer_ephem(ix, &payer, &ctx, &mut validator);
        let counter =
            fetch_counter_ephem(&ctx, &payer.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 6,
                updates: 2,
                label: COUNTER.to_string()
            },
            cleanup(&mut validator)
        );
        debug!("✅ Multiplied counter in ephemeral");
    }

    {
        // Increment counter in ephemeral again and commit it
        wait_for_ledger_persist(&ctx, &mut validator);

        let ix = create_add_and_schedule_commit_ix(payer.pubkey(), 4, false);
        let sig = confirm_tx_with_payer_ephem(ix, &payer, &ctx, &mut validator);

        let res = expect!(
            ctx.fetch_schedule_commit_result::<FlexiCounter>(sig),
            validator
        );
        let counter_ephem = expect!(
            res.included.values().next().ok_or("missing counter"),
            validator
        );

        // NOTE: we need to wait for the commit transaction on chain to confirm
        // before we can check the counter data there
        expect!(res.confirm_commit_transactions_on_chain(&ctx), validator);
        let counter_chain =
            fetch_counter_chain(&payer.pubkey(), &mut validator);

        assert_eq!(
            counter_ephem,
            &FlexiCounter {
                count: 10,
                updates: 3,
                label: COUNTER.to_string()
            },
            cleanup(&mut validator)
        );

        assert_eq!(
            counter_chain,
            FlexiCounter {
                count: 10,
                updates: 3,
                label: COUNTER.to_string()
            },
            cleanup(&mut validator)
        );
        debug!("✅ Incremented and committed counter in ephemeral");
    }

    // Ensure that at this point we only have three chain transactions
    // for the counter, showing that the commits didn't get sent to chain again:
    // - init
    // - delegate
    // - commit (original from while validator was running)
    assert_counter_commits_on_chain(&ctx, &mut validator, &payer.pubkey(), 3);

    let slot = wait_for_ledger_persist(&ctx, &mut validator);
    (validator, slot, payer)
}

fn read(ledger_path: &Path, payer: &Pubkey) -> Child {
    let programs = get_programs_with_flexi_counter();

    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        Some(programs),
        false,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    wait_for_cloned_accounts_hydration();

    let counter_ephem = fetch_counter_ephem(&ctx, payer, &mut validator);
    assert_eq!(
        counter_ephem,
        FlexiCounter {
            count: 10,
            updates: 3,
            label: COUNTER.to_string()
        },
        cleanup(&mut validator)
    );
    debug!("✅ Verified counter on chain state after restore");

    let counter_chain = fetch_counter_chain(payer, &mut validator);
    assert_eq!(
        counter_chain,
        FlexiCounter {
            count: 10,
            updates: 3,
            label: COUNTER.to_string()
        },
        cleanup(&mut validator)
    );

    debug!("✅ Verified counter ephemeral state after restore");

    // Ensure that at this point we still only have three chain transactions
    // for the counter, showing that the commits didn't get sent to chain again.
    assert_counter_commits_on_chain(&ctx, &mut validator, payer, 3);

    debug!("✅ Verified counter commits on chain after restore");

    validator
}
