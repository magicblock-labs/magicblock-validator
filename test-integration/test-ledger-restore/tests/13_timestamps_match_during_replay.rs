use cleanass::assert_eq;
use magicblock_config::LedgerResumeStrategy;
use solana_transaction_status::UiTransactionEncoding;
use std::{path::Path, process::Child, thread::sleep, time::Duration};

use integration_test_tools::{
    expect, tmpdir::resolve_tmp_dir, validator::cleanup,
};
use solana_sdk::{pubkey::Pubkey, signature::Signature};
use test_ledger_restore::{setup_offline_validator, TMP_DIR_LEDGER};

// In this test we ensure that the timestamps of the blocks in the restored
// ledger match the timestamps of the blocks in the original ledger.

const SNAPSHOT_FREQUENCY: u64 = 2;

#[test]
fn restore_preserves_timestamps() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let pubkey = Pubkey::new_unique();

    let (mut validator, slot, signature, block_time) =
        write(&ledger_path, &pubkey);
    validator.kill().unwrap();

    assert!(slot > SNAPSHOT_FREQUENCY);

    let mut validator = read(&ledger_path, signature, block_time);
    validator.kill().unwrap();
}

fn write(ledger_path: &Path, pubkey: &Pubkey) -> (Child, u64, Signature, i64) {
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        None,
        LedgerResumeStrategy::Reset,
        false,
    );

    // First airdrop followed by wait until account is flushed
    let signature = expect!(ctx.airdrop_ephem(pubkey, 1_111_111), validator);

    // Block time is only set after the slot
    expect!(ctx.wait_for_next_slot_ephem(), validator);

    let block_time = expect!(
        ctx.try_ephem_client().and_then(|client| {
            client
                .get_transaction(&signature, UiTransactionEncoding::Base58)
                .map_err(|e| anyhow::anyhow!("Get transaction failed: {}", e))
                .and_then(|tx| {
                    tx.block_time.ok_or(anyhow::anyhow!("No block time"))
                })
        }),
        validator
    );

    // Wait for the first slot after the snapshot
    let slot = loop {
        let slot = ctx.get_slot_ephem().unwrap();
        if slot % SNAPSHOT_FREQUENCY == 1 {
            break slot;
        }
        // Sleep for half a slot
        sleep(Duration::from_millis(25));
    };

    (validator, slot, signature, block_time)
}

fn read(ledger_path: &Path, signature: Signature, block_time: i64) -> Child {
    // Measure time
    let _ = std::time::Instant::now();
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        None,
        LedgerResumeStrategy::Replay,
        false,
    );
    eprintln!(
        "Validator started in {:?}",
        std::time::Instant::now().elapsed()
    );

    let restored_block_time = expect!(
        ctx.try_ephem_client().and_then(|client| {
            client
                .get_transaction(&signature, UiTransactionEncoding::Base58)
                .map_err(|e| anyhow::anyhow!("Get transaction failed: {}", e))
                .and_then(|tx| {
                    tx.block_time.ok_or(anyhow::anyhow!("No block time"))
                })
        }),
        validator
    );
    assert_eq!(restored_block_time, block_time, cleanup(&mut validator));
    validator
}
