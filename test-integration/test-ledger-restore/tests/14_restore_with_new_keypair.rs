use cleanass::assert_eq;
use magicblock_accounts_db::config::TEST_SNAPSHOT_FREQUENCY;
use std::{path::Path, process::Child};

use integration_test_tools::{
    expect, loaded_accounts::LoadedAccounts, tmpdir::resolve_tmp_dir,
};
use solana_sdk::pubkey::Pubkey;
use test_ledger_restore::{
    cleanup, setup_offline_validator, wait_for_ledger_persist, TMP_DIR_LEDGER,
};

// In this test we ensure that restoring from a later slot by hydrating the
// bank with flushed accounts state works.
// First we airdrop to an account, then wait until the state of
// the account should have been flushed to disk.
// Then we airdrop again.
// The ledger restore will start from a slot after the first airdrop was
// flushed.

#[test]
fn restore_ledger_with_new_validator_authority() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let pubkeys = (0..10).map(|_| Pubkey::new_unique()).collect::<Vec<_>>();

    let (mut validator, slot) = write(&ledger_path, &pubkeys);
    validator.kill().unwrap();

    assert!(slot > TEST_SNAPSHOT_FREQUENCY);

    let mut validator = read(&ledger_path, &pubkeys);
    validator.kill().unwrap();
}

fn write(ledger_path: &Path, pubkeys: &[Pubkey]) -> (Child, u64) {
    let loaded_chain_accounts =
        LoadedAccounts::new_with_new_validator_authority();
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        None,
        true,
        true,
        &loaded_chain_accounts,
    );

    // Bunch of transactions followed by wait until account is flushed
    for pubkey in pubkeys {
        expect!(ctx.airdrop_ephem(&pubkey, 1_111_111), validator);

        // NOTE: This slows the test down a lot (500 * 50ms = 25s) and will
        // be improved once we can configure `FLUSH_ACCOUNTS_SLOT_FREQ`
        expect!(
            ctx.wait_for_delta_slot_ephem(TEST_SNAPSHOT_FREQUENCY),
            validator
        );
    }

    let slot = wait_for_ledger_persist(&mut validator);

    (validator, slot)
}

fn read(ledger_path: &Path, pubkeys: &[Pubkey]) -> Child {
    let loaded_chain_accounts =
        LoadedAccounts::new_with_new_validator_authority();
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        None,
        false,
        false,
        &loaded_chain_accounts,
    );

    for pubkey in pubkeys {
        let lamports =
            expect!(ctx.fetch_ephem_account_balance(pubkey), validator);
        assert_eq!(lamports, 1_111_111, cleanup(&mut validator));
    }

    validator
}
