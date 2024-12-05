use cleanass::assert_eq;
use std::{path::Path, process::Child};

use integration_test_tools::{expect, tmpdir::resolve_tmp_dir};
use magicblock_config::ProgramConfig;
use program_flexi_counter::{
    instruction::{create_add_ix, create_init_ix, create_mul_ix},
    state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer,
};
use test_ledger_restore::{
    cleanup, confirm_tx_with_payer_ephem, fetch_counter_ephem,
    setup_offline_validator, wait_for_ledger_persist, FLEXI_COUNTER_ID,
    TMP_DIR_LEDGER,
};

const SLOT_MS: u64 = 150;

fn payer1_keypair() -> Keypair {
    Keypair::from_base58_string("M8CcAuQHVQj91sKW68prBjNzvhEVjTj1ADMDej4KJTuwF4ckmibCmX3U6XGTMfGX5g7Xd43EXSNcjPkUWWcJpWA")
}
fn payer2_keypair() -> Keypair {
    Keypair::from_base58_string("j5cwGmb19aNqc1Mc1n2xUSvZkG6vxjsYPHhLJC6RYmQbS1ggWeEU57jCnh5QwbrTzaCnDLE4UaS2wTVBWYyq5KT")
}

/*
* This test uses flexi counter program which is loaded at validator startup.
* It then executes math operations on the counter which only result in the same
* outcome if they are executed in the correct order.
* This way we ensure that during ledger replay the order of transactions is
* the same as when it was recorded
*/

#[test]
fn restore_ledger_with_flexi_counter_same_slot() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let payer1 = payer1_keypair();
    let payer2 = payer2_keypair();

    let (mut validator, _) = write(&ledger_path, &payer1, &payer2, false);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &payer1.pubkey(), &payer2.pubkey());
    validator.kill().unwrap();
}

#[test]
fn restore_ledger_with_flexi_counter_separate_slot() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let payer1 = payer1_keypair();
    let payer2 = payer2_keypair();

    let (mut validator, _) = write(&ledger_path, &payer1, &payer2, true);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &payer1.pubkey(), &payer2.pubkey());
    validator.kill().unwrap();
}

fn get_programs() -> Vec<ProgramConfig> {
    vec![ProgramConfig {
        id: FLEXI_COUNTER_ID.try_into().unwrap(),
        path: "program_flexi_counter.so".to_string(),
    }]
}

fn write(
    ledger_path: &Path,
    payer1: &Keypair,
    payer2: &Keypair,
    separate_slot: bool,
) -> (Child, u64) {
    const COUNTER1: &str = "Counter of Payer 1";
    const COUNTER2: &str = "Counter of Payer 2";

    let programs = get_programs();

    // Choosing slower slots in order to have the airdrop + transaction occur in the
    // same slot and ensure that they are replayed in the correct order
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        Some(programs),
        Some(SLOT_MS),
        true,
    );

    expect!(ctx.wait_for_slot_ephem(1), validator);

    // Airdrop to payers
    expect!(
        ctx.airdrop_ephem(&payer1.pubkey(), LAMPORTS_PER_SOL),
        validator
    );
    if separate_slot {
        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }
    expect!(
        ctx.airdrop_ephem(&payer2.pubkey(), LAMPORTS_PER_SOL),
        validator
    );

    {
        // Create and send init counter1 instruction
        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }

        let ix = create_init_ix(payer1.pubkey(), COUNTER1.to_string());
        confirm_tx_with_payer_ephem(ix, payer1, &mut validator);
        let counter = fetch_counter_ephem(&payer1.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 0,
                updates: 0,
                label: COUNTER1.to_string()
            },
            cleanup(&mut validator)
        );
    }

    {
        // Execute ((0) + 5) * 2 on counter1
        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }
        let ix_add = create_add_ix(payer1.pubkey(), 5);
        let ix_mul = create_mul_ix(payer1.pubkey(), 2);
        confirm_tx_with_payer_ephem(ix_add, payer1, &mut validator);

        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }
        confirm_tx_with_payer_ephem(ix_mul, payer1, &mut validator);

        let counter = fetch_counter_ephem(&payer1.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 10,
                updates: 2,
                label: COUNTER1.to_string()
            },
            cleanup(&mut validator)
        );
    }

    {
        // Create and send init counter1 instruction
        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }

        let ix = create_init_ix(payer2.pubkey(), COUNTER2.to_string());
        confirm_tx_with_payer_ephem(ix, payer2, &mut validator);
        let counter = fetch_counter_ephem(&payer2.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 0,
                updates: 0,
                label: COUNTER2.to_string()
            },
            cleanup(&mut validator)
        );
    }

    {
        // Add 9 to counter 2
        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }
        let ix_add = create_add_ix(payer2.pubkey(), 9);
        confirm_tx_with_payer_ephem(ix_add, payer2, &mut validator);

        let counter = fetch_counter_ephem(&payer2.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 9,
                updates: 1,
                label: COUNTER2.to_string()
            },
            cleanup(&mut validator)
        );
    }

    {
        // Add 3 to counter 1
        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }
        let ix_add = create_add_ix(payer1.pubkey(), 3);
        confirm_tx_with_payer_ephem(ix_add, payer1, &mut validator);

        let counter = fetch_counter_ephem(&payer1.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 13,
                updates: 3,
                label: COUNTER1.to_string()
            },
            cleanup(&mut validator)
        );
    }

    {
        // Multiply counter 2 with 3
        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }
        let ix_add = create_mul_ix(payer2.pubkey(), 3);
        confirm_tx_with_payer_ephem(ix_add, payer2, &mut validator);

        let counter = fetch_counter_ephem(&payer2.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 27,
                updates: 2,
                label: COUNTER2.to_string()
            },
            cleanup(&mut validator)
        );
    }

    let slot = wait_for_ledger_persist(&mut validator);

    (validator, slot)
}

fn read(ledger_path: &Path, payer1: &Pubkey, payer2: &Pubkey) -> Child {
    let programs = get_programs();
    let (_, mut validator, _) = setup_offline_validator(
        ledger_path,
        Some(programs),
        Some(SLOT_MS),
        false,
    );

    let counter1_decoded = fetch_counter_ephem(payer1, &mut validator);
    assert_eq!(
        counter1_decoded,
        FlexiCounter {
            count: 13,
            updates: 3,
            label: "Counter of Payer 1".to_string(),
        },
        cleanup(&mut validator)
    );

    let counter2_decoded = fetch_counter_ephem(payer2, &mut validator);
    assert_eq!(
        counter2_decoded,
        FlexiCounter {
            count: 27,
            updates: 2,
            label: "Counter of Payer 2".to_string(),
        },
        cleanup(&mut validator)
    );

    validator
}

// -----------------
// Diagnose
// -----------------
// Uncomment either of the below to run ledger write/read in isolation and
// optionally keep the validator running after reading the ledger
// #[test]
fn _flexi_counter_diagnose_write() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let payer1 = payer1_keypair();
    let payer2 = payer2_keypair();

    let (mut validator, slot) = write(&ledger_path, &payer1, &payer2, true);

    eprintln!("{}", ledger_path.display());
    eprintln!("slot: {}", slot);

    let counter1_decoded =
        fetch_counter_ephem(&payer1.pubkey(), &mut validator);
    eprint!("1: {:#?}", counter1_decoded);

    let counter2_decoded =
        fetch_counter_ephem(&payer2.pubkey(), &mut validator);
    eprint!("2: {:#?}", counter2_decoded);

    validator.kill().unwrap();
}

// #[test]
fn _flexi_counter_diagnose_read() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let payer1 = payer1_keypair();
    let payer2 = payer2_keypair();

    let mut validator = read(&ledger_path, &payer1.pubkey(), &payer2.pubkey());

    eprintln!("{}", ledger_path.display());

    let counter1_decoded =
        fetch_counter_ephem(&payer1.pubkey(), &mut validator);
    eprint!("1: {:#?}", counter1_decoded);

    let counter2_decoded =
        fetch_counter_ephem(&payer2.pubkey(), &mut validator);
    eprint!("2: {:#?}", counter2_decoded);

    validator.kill().unwrap();
}
