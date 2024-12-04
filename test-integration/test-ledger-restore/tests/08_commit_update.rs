use cleanass::assert_eq;
use std::{path::Path, process::Child};

use integration_test_tools::{expect, tmpdir::resolve_tmp_dir};
use program_flexi_counter::instruction::{
    create_add_and_schedule_commit_ix, create_add_ix, create_mul_ix,
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
const COUNTER: &str = "Counter of Payer";
fn payer_keypair() -> Keypair {
    Keypair::new()
}

// In this test we update a delegated account in the ephemeral, commit it and
// then update it again.
// We then restore the ledger and verify that the committed account available
// with the last update and that the commit was not run during ledger processing.
// Finally after the ledger is restored we want to ensure that we can keep updating
// the account. This part is currently not yet supported.
//
// NOTE: that most of the setup is similar to 07_commit_delegated_account.rs
// except that we removed the intermediate checks.

#[test]
fn restore_ledger_committed_and_updated_account() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let payer = payer_keypair();

    let (mut validator, _) = write(&ledger_path, &payer);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &payer);
    validator.kill().unwrap();
}

fn write(ledger_path: &Path, payer: &Keypair) -> (Child, u64) {
    let programs = get_programs_with_flexi_counter();

    let (_, mut validator, ctx) =
        setup_validator_with_local_remote(ledger_path, Some(programs), true);

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

    // Increment counter in ephemeral and commit it
    {
        wait_for_ledger_persist(&mut validator);

        let ix = create_add_and_schedule_commit_ix(payer.pubkey(), 4, false);
        let sig = confirm_tx_with_payer_ephem(ix, payer, &mut validator);

        let res = ctx
            .fetch_schedule_commit_result::<FlexiCounter>(sig)
            .unwrap();
        expect!(res.confirm_commit_transactions_on_chain(&ctx), validator);

        let counter_ephem = expect!(
            res.included.values().next().ok_or("missing counter"),
            validator
        );

        expect!(res.confirm_commit_transactions_on_chain(&ctx), validator);
        let counter_chain =
            fetch_counter_chain(&payer.pubkey(), &mut validator);

        assert_eq!(
            counter_ephem,
            &FlexiCounter {
                count: 4,
                updates: 1,
                label: COUNTER.to_string()
            },
            cleanup(&mut validator)
        );

        assert_eq!(
            counter_chain,
            FlexiCounter {
                count: 4,
                updates: 1,
                label: COUNTER.to_string()
            },
            cleanup(&mut validator)
        );
    }

    // Multiply counter in ephemeral (after we committed it) and verify
    // it is updated in the ephemeral only
    {
        confirm_tx_with_payer_ephem(
            create_mul_ix(payer.pubkey(), 2),
            payer,
            &mut validator,
        );

        let counter_ephem =
            fetch_counter_ephem(&payer.pubkey(), &mut validator);
        let counter_chain =
            fetch_counter_chain(&payer.pubkey(), &mut validator);
        assert_eq!(
            counter_ephem,
            FlexiCounter {
                count: 8,
                updates: 2,
                label: COUNTER.to_string()
            },
            cleanup(&mut validator)
        );

        assert_eq!(
            counter_chain,
            FlexiCounter {
                count: 4,
                updates: 1,
                label: COUNTER.to_string()
            },
            cleanup(&mut validator)
        );
    }

    assert_counter_commits_on_chain(&ctx, &mut validator, &payer.pubkey(), 3);

    let slot = wait_for_ledger_persist(&mut validator);
    (validator, slot)
}

fn read(ledger_path: &Path, payer_kp: &Keypair) -> Child {
    let payer = &payer_kp.pubkey();
    let programs = get_programs_with_flexi_counter();

    let (_, mut validator, ctx) =
        setup_validator_with_local_remote(ledger_path, Some(programs), false);

    let counter_ephem = fetch_counter_ephem(payer, &mut validator);
    let counter_chain = fetch_counter_chain(payer, &mut validator);
    assert_eq!(
        counter_ephem,
        FlexiCounter {
            count: 8,
            updates: 2,
            label: COUNTER.to_string()
        },
        cleanup(&mut validator)
    );
    assert_eq!(
        counter_chain,
        FlexiCounter {
            count: 4,
            updates: 1,
            label: COUNTER.to_string()
        },
        cleanup(&mut validator)
    );

    // Ensure we can use the counter as before and increase its count
    let ix = create_add_ix(payer_kp.pubkey(), 3);
    confirm_tx_with_payer_ephem(ix, payer_kp, &mut validator);
    let counter = fetch_counter_ephem(payer, &mut validator);
    assert_eq!(
        counter,
        FlexiCounter {
            count: 7,
            updates: 2,
            label: COUNTER.to_string()
        },
        cleanup(&mut validator)
    );

    // Ensure we did not commit during ledger replay
    assert_counter_commits_on_chain(&ctx, &mut validator, payer, 3);

    validator
}
