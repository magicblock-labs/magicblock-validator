use std::{path::Path, process::Child};

use integration_test_tools::{expect, tmpdir::resolve_tmp_dir};
use program_flexi_counter::instruction::{
    create_add_and_schedule_commit_ix, create_add_ix, create_mul_ix,
};
use program_flexi_counter::{
    delegation_program_id,
    instruction::{create_delegate_ix, create_init_ix},
    state::FlexiCounter,
};
use sleipnir_config::ProgramConfig;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer,
};
use test_ledger_restore::{
    confirm_tx_with_payer_chain, confirm_tx_with_payer_ephem,
    fetch_counter_chain, fetch_counter_ephem, fetch_counter_owner_chain,
    setup_validator_with_local_remote, wait_for_ledger_persist,
    FLEXI_COUNTER_ID, TMP_DIR_LEDGER,
};

const COUNTER: &str = "Counter of Payer";
fn payer_keypair() -> Keypair {
    Keypair::new()
}

fn get_programs() -> Vec<ProgramConfig> {
    vec![ProgramConfig {
        id: FLEXI_COUNTER_ID.try_into().unwrap(),
        path: "program_flexi_counter.so".to_string(),
    }]
}

#[test]
fn restore_ledger_containing_delegated_and_committed_account() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let payer = payer_keypair();

    let (mut validator, _) = write(&ledger_path, &payer);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &payer.pubkey());
    validator.kill().unwrap();
}

fn write(ledger_path: &Path, payer: &Keypair) -> (Child, u64) {
    let programs = get_programs();

    let (_, mut validator, ctx) =
        setup_validator_with_local_remote(ledger_path, Some(programs), true);

    // Airdrop to payer on chain
    expect!(
        ctx.airdrop_chain(&payer.pubkey(), LAMPORTS_PER_SOL),
        validator
    );

    {
        // Create and send init counter instruction on chain
        let ix = create_init_ix(payer.pubkey(), COUNTER.to_string());
        confirm_tx_with_payer_chain(ix, payer, &mut validator);
        let counter = fetch_counter_chain(&payer.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 0,
                updates: 0,
                label: COUNTER.to_string()
            }
        )
    }
    {
        // Delegate counter to ephemeral
        let ix = create_delegate_ix(payer.pubkey());
        confirm_tx_with_payer_chain(ix, payer, &mut validator);
        let owner = fetch_counter_owner_chain(&payer.pubkey(), &mut validator);
        assert_eq!(owner, delegation_program_id());
    }

    {
        // Increment counter in ephemeral
        let ix = create_add_ix(payer.pubkey(), 3);
        confirm_tx_with_payer_ephem(ix, payer, &mut validator);
        let counter = fetch_counter_ephem(&payer.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 3,
                updates: 1,
                label: COUNTER.to_string()
            }
        )
    }

    {
        // Multiply counter in ephemeral
        let ix = create_mul_ix(payer.pubkey(), 2);
        confirm_tx_with_payer_ephem(ix, payer, &mut validator);
        let counter = fetch_counter_ephem(&payer.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 6,
                updates: 2,
                label: COUNTER.to_string()
            }
        )
    }

    {
        // Increment counter in ephemeral again and commit it
        wait_for_ledger_persist(&mut validator);

        let ix = create_add_and_schedule_commit_ix(payer.pubkey(), 4, false);
        let sig = confirm_tx_with_payer_ephem(ix, payer, &mut validator);

        let res = ctx
            .fetch_schedule_commit_result::<FlexiCounter>(sig)
            .unwrap();
        expect!(res.confirm_commit_transactions_on_chain(&ctx), validator);

        let counter = expect!(
            res.included.values().next().ok_or("missing counter"),
            validator
        );
        let counter_ephem = expect!(
            counter
                .ephem_account
                .as_ref()
                .ok_or("missing ephem account"),
            validator
        );
        let counter_chain = expect!(
            counter
                .chain_account
                .as_ref()
                .ok_or("missing chain account"),
            validator
        );

        assert_eq!(
            counter_ephem,
            &FlexiCounter {
                count: 10,
                updates: 3,
                label: COUNTER.to_string()
            }
        );

        assert_eq!(
            counter_chain,
            &FlexiCounter {
                count: 10,
                updates: 3,
                label: COUNTER.to_string()
            }
        );
    }

    {
        // Ensure that at this point three chain transactions exist for the counter:
        // - init
        // - delegate
        // - commit
        let (pda, _) = FlexiCounter::pda(&payer.pubkey());
        let stats =
            expect!(ctx.get_signaturestats_for_address_chain(&pda), validator);
        assert_eq!(stats.len(), 3);
    }

    let slot = wait_for_ledger_persist(&mut validator);
    (validator, slot)
}

fn read(ledger_path: &Path, payer: &Pubkey) -> Child {
    let programs = get_programs();

    let (_, mut validator, ctx) =
        setup_validator_with_local_remote(ledger_path, Some(programs), false);

    let counter_ephem = fetch_counter_ephem(payer, &mut validator);
    assert_eq!(
        counter_ephem,
        FlexiCounter {
            count: 10,
            updates: 3,
            label: COUNTER.to_string()
        }
    );

    let counter_chain = fetch_counter_chain(payer, &mut validator);
    assert_eq!(
        counter_chain,
        FlexiCounter {
            count: 10,
            updates: 3,
            label: COUNTER.to_string()
        }
    );

    {
        // Wait long enough for scheduled commits to have been handled
        expect!(ctx.wait_for_next_slot_ephem(), validator);
        expect!(ctx.wait_for_next_slot_ephem(), validator);
        expect!(ctx.wait_for_next_slot_ephem(), validator);

        // Ensure that at this point we still only have three chain transactions
        // for the counter, showing that the commits didn't get sent to chain again:
        // - init
        // - delegate
        // - commit (original from while validator was running)
        let (pda, _) = FlexiCounter::pda(payer);
        let stats =
            expect!(ctx.get_signaturestats_for_address_chain(&pda), validator);
        assert_eq!(stats.len(), 3);
    }
    validator
}
