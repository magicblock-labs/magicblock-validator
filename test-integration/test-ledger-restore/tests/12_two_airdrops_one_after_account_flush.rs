use cleanass::assert_eq;
use magicblock_config::LedgerResumeStrategy;
use std::{path::Path, process::Child};

use integration_test_tools::{
    expect, tmpdir::resolve_tmp_dir, validator::cleanup,
};
use solana_sdk::pubkey::Pubkey;
use test_ledger_restore::{
    setup_offline_validator, wait_for_ledger_persist, TMP_DIR_LEDGER,
};

// In this test we ensure that restoring from a later slot by hydrating the
// bank with flushed accounts state works.
// First we airdrop to an account, then wait until the state of
// the account should have been flushed to disk.
// Then we airdrop again.
// The ledger restore will start from a slot after the first airdrop was
// flushed.

const SNAPSHOT_FREQUENCY: u64 = 2;

#[test]
fn restore_ledger_with_two_airdrops_with_account_flush_in_between() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let pubkey = Pubkey::new_unique();

    let (mut validator, slot) = write(&ledger_path, &pubkey);
    validator.kill().unwrap();

    assert!(slot > SNAPSHOT_FREQUENCY);

    let mut validator = read(&ledger_path, &pubkey);
    validator.kill().unwrap();
}

fn write(ledger_path: &Path, pubkey: &Pubkey) -> (Child, u64) {
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        None,
        LedgerResumeStrategy::Reset,
        false,
    );

    // First airdrop followed by wait until account is flushed
    {
        expect!(ctx.airdrop_ephem(pubkey, 1_111_111), validator);
        let lamports =
            expect!(ctx.fetch_ephem_account_balance(pubkey), validator);
        assert_eq!(lamports, 1_111_111, cleanup(&mut validator));

        // Snapshot frequency is set to 2 slots for the offline validator
        expect!(
            ctx.wait_for_delta_slot_ephem(SNAPSHOT_FREQUENCY + 1),
            validator
        );
    }
    // Second airdrop
    {
        expect!(ctx.airdrop_ephem(pubkey, 2_222_222), validator);
        let lamports =
            expect!(ctx.fetch_ephem_account_balance(pubkey), validator);
        assert_eq!(lamports, 3_333_333, cleanup(&mut validator));
    }
    let slot = wait_for_ledger_persist(&mut validator);

    (validator, slot)
}

fn read(ledger_path: &Path, pubkey: &Pubkey) -> Child {
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

    let lamports = expect!(ctx.fetch_ephem_account_balance(pubkey), validator);
    assert_eq!(lamports, 3_333_333, cleanup(&mut validator));
    validator
}
