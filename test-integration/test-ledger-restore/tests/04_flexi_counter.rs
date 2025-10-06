use log::*;
use std::{path::Path, process::Child};
use test_kit::init_logger;

use cleanass::assert_eq;
use integration_test_tools::{
    expect, tmpdir::resolve_tmp_dir, validator::cleanup,
};
use magicblock_config::LedgerResumeStrategy;
use program_flexi_counter::{
    instruction::{create_add_ix, create_mul_ix},
    state::FlexiCounter,
};
use solana_sdk::{pubkey::Pubkey, signer::Signer};
use test_ledger_restore::{
    confirm_tx_with_payer_ephem, fetch_counter_ephem,
    init_and_delegate_counter_and_payer, setup_offline_validator,
    setup_validator_with_local_remote_and_resume_strategy,
    wait_for_ledger_persist, TMP_DIR_LEDGER,
};

const SLOT_MS: u64 = 150;

/*
* This test uses flexi counter program which is loaded at validator startup.
* It then executes math operations on the counter which only result in the same
* outcome if they are executed in the correct order.
* This way we ensure that during ledger replay the order of transactions is
* the same as when it was recorded
*/

/* TODO: @@@ FAILING
*
[2025-10-06T13:48:09.888935Z WARN  integration_test_tools::transactions] Simulation Result: 4sG6rbQEBCkuZkMcJFuamkwpQBMdT6zEFueVLSXHC1Ao7GCoXHB1uKhUboqJtYK3fWZLZkXdJyaKX8SZQHTX1sZV

    Replacement Blockhash: RpcBlockhash { blockhash: "4zpH8xdPfdvRKr9Gr6zzqDTknqqCrfDYK8eLM7Uukmea", last_valid_block_height: 1355 }

    Error: InvalidWritableAccount

=> Does not confirm transaction
*/
#[test]
fn test_restore_ledger_with_flexi_counter_same_slot() {
    init_logger!();
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (mut validator, _, payer1, payer2) = write(&ledger_path, false);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &payer1, &payer2);
    validator.kill().unwrap();
}

#[ignore]
#[test]
fn test_restore_ledger_with_flexi_counter_separate_slot() {
    init_logger!();

    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (mut validator, _, payer1, payer2) = write(&ledger_path, true);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &payer1, &payer2);
    validator.kill().unwrap();
}

fn write(
    ledger_path: &Path,
    separate_slot: bool,
) -> (Child, u64, Pubkey, Pubkey) {
    const COUNTER1: &str = "Counter of Payer 1";
    const COUNTER2: &str = "Counter of Payer 2";

    // Choosing slower slots in order to have the airdrop + transaction occur in the
    // same slot and ensure that they are replayed in the correct order
    let (_, mut validator, ctx) =
        setup_validator_with_local_remote_and_resume_strategy(
            ledger_path,
            None,
            LedgerResumeStrategy::Reset {
                slot: 0,
                keep_accounts: false,
            },
            true,
            &Default::default(),
        );

    expect!(ctx.wait_for_slot_ephem(1), validator);

    let (payer1, counter1_pda) = {
        // Create and send init counter1 instruction
        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }
        init_and_delegate_counter_and_payer(&ctx, &mut validator, COUNTER1)
    };
    debug!(
        "✅ Delegated counter {counter1_pda} for {}",
        payer1.pubkey()
    );

    let (payer2, counter2_pda) = {
        // Create and send init counter2 instruction
        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }
        init_and_delegate_counter_and_payer(&ctx, &mut validator, COUNTER2)
    };
    debug!(
        "✅ Delegated counter {counter2_pda} for {}",
        payer2.pubkey()
    );

    {
        // Execute ((0) + 5) * 2 on counter1
        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }
        let ix_add = create_add_ix(payer1.pubkey(), 5);
        let ix_mul = create_mul_ix(payer1.pubkey(), 2);
        confirm_tx_with_payer_ephem(ix_add, &payer1, &ctx, &mut validator);
        debug!("✅ Added 5 to counter1 {counter1_pda}");

        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }
        confirm_tx_with_payer_ephem(ix_mul, &payer1, &ctx, &mut validator);
        debug!("✅ Multiplied 2 for counter1 {counter1_pda}");

        let counter =
            fetch_counter_ephem(&ctx, &payer1.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 10,
                updates: 2,
                label: COUNTER1.to_string()
            },
            cleanup(&mut validator)
        );
        debug!("✅ Verified counter1 state {counter1_pda}");
    }

    {
        // Add 9 to counter 2
        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }
        let ix_add = create_add_ix(payer2.pubkey(), 9);
        confirm_tx_with_payer_ephem(ix_add, &payer2, &ctx, &mut validator);

        let counter =
            fetch_counter_ephem(&ctx, &payer2.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 9,
                updates: 1,
                label: COUNTER2.to_string()
            },
            cleanup(&mut validator)
        );
        debug!("✅ Added 9 to counter2 {counter2_pda}");
    }

    {
        // Add 3 to counter 1
        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }
        let ix_add = create_add_ix(payer1.pubkey(), 3);
        confirm_tx_with_payer_ephem(ix_add, &payer1, &ctx, &mut validator);

        let counter =
            fetch_counter_ephem(&ctx, &payer1.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 13,
                updates: 3,
                label: COUNTER1.to_string()
            },
            cleanup(&mut validator)
        );
        debug!("✅ Added 3 to counter1 {counter1_pda}");
    }

    {
        // Multiply counter 2 with 3
        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }
        let ix_add = create_mul_ix(payer2.pubkey(), 3);
        confirm_tx_with_payer_ephem(ix_add, &payer2, &ctx, &mut validator);

        let counter =
            fetch_counter_ephem(&ctx, &payer2.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 27,
                updates: 2,
                label: COUNTER2.to_string()
            },
            cleanup(&mut validator)
        );
        debug!("✅ Multiplied 3 for counter2 {counter1_pda}");
    }

    let slot = wait_for_ledger_persist(&ctx, &mut validator);

    (validator, slot, payer1.pubkey(), payer2.pubkey())
}

fn read(ledger_path: &Path, payer1: &Pubkey, payer2: &Pubkey) -> Child {
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        Some(SLOT_MS),
        LedgerResumeStrategy::Resume { replay: true },
        false,
    );

    let counter1_decoded = fetch_counter_ephem(&ctx, payer1, &mut validator);
    assert_eq!(
        counter1_decoded,
        FlexiCounter {
            count: 13,
            updates: 3,
            label: "Counter of Payer 1".to_string(),
        },
        cleanup(&mut validator)
    );
    debug!("✅ Verified counter1 state after restore");

    let counter2_decoded = fetch_counter_ephem(&ctx, payer2, &mut validator);
    assert_eq!(
        counter2_decoded,
        FlexiCounter {
            count: 27,
            updates: 2,
            label: "Counter of Payer 2".to_string(),
        },
        cleanup(&mut validator)
    );
    debug!("✅ Verified counter2 state after restore");

    validator
}
