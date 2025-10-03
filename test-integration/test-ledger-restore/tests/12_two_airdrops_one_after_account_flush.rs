use std::{path::Path, process::Child};

use cleanass::assert_eq;
use integration_test_tools::{
    expect, loaded_accounts::LoadedAccounts, tmpdir::resolve_tmp_dir,
    validator::cleanup,
};
use log::*;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};
use test_kit::init_logger;
use test_ledger_restore::{
    airdrop_and_delegate_accounts, setup_validator_with_local_remote,
    transfer_lamports, wait_for_ledger_persist, SNAPSHOT_FREQUENCY,
    TMP_DIR_LEDGER,
};

// In this test we ensure that restoring from a later slot by hydrating the
// bank with flushed accounts state works.
// First we airdrop to an account, then wait until the state of
// the account should have been flushed to disk.
// Then we airdrop again.
// The ledger restore will start from a slot after the first airdrop was
// flushed.

#[test]
fn test_restore_ledger_with_two_airdrops_with_account_flush_in_between() {
    init_logger!();

    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (mut validator, slot, keypair) = write(&ledger_path);
    validator.kill().unwrap();

    assert!(slot > SNAPSHOT_FREQUENCY);

    let mut validator = read(&ledger_path, &keypair.pubkey());
    validator.kill().unwrap();
}

fn write(ledger_path: &Path) -> (Child, u64, Keypair) {
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        true,
        true,
        &LoadedAccounts::default(),
    );

    // Wait to make sure we don't process transactions on slot 0
    expect!(ctx.wait_for_next_slot_ephem(), validator);

    // Airdrop and delegate account on chain
    let mut keypairs = airdrop_and_delegate_accounts(
        &ctx,
        &mut validator,
        &[1_111_111, 1_000_000],
    );
    let transfer_payer = keypairs.drain(0..1).next().unwrap();
    let transfer_receiver = keypairs.drain(0..1).next().unwrap();
    debug!(
        "✅ Airdropped and delegated payer {} and receiver {} on chain",
        transfer_payer.pubkey(),
        transfer_receiver.pubkey()
    );

    // First transfer followed by wait until account is flushed
    {
        transfer_lamports(
            &ctx,
            &mut validator,
            &transfer_payer,
            &transfer_receiver.pubkey(),
            0_000_111,
        );
        let lamports = expect!(
            ctx.fetch_ephem_account_balance(&transfer_receiver.pubkey()),
            validator
        );
        assert_eq!(lamports, 1_000_111, cleanup(&mut validator));
        debug!(
            "✅ First transfer complete, balance {} now has {} lamports",
            transfer_receiver.pubkey(),
            lamports
        );

        // Snapshot frequency is set to 2 slots for the validator
        expect!(
            ctx.wait_for_delta_slot_ephem(SNAPSHOT_FREQUENCY + 1),
            validator
        );
        debug!("✅ Waited for account flush after first transfer");
    }

    // Second transfer
    {
        transfer_lamports(
            &ctx,
            &mut validator,
            &transfer_payer,
            &transfer_receiver.pubkey(),
            0_111_000,
        );
        let lamports = expect!(
            ctx.fetch_ephem_account_balance(&transfer_receiver.pubkey()),
            validator
        );
        assert_eq!(lamports, 1_111_111, cleanup(&mut validator));
        debug!(
            "✅ Second transfer complete, balance {} now has {} lamports",
            transfer_receiver.pubkey(),
            lamports
        );
    }

    let slot = wait_for_ledger_persist(&mut validator);
    debug!("✅ Ledger persisted at slot {}", slot);

    (validator, slot, transfer_receiver)
}

fn read(ledger_path: &Path, pubkey: &Pubkey) -> Child {
    // Measure time
    let start = std::time::Instant::now();
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        false,
        false,
        &LoadedAccounts::default(),
    );
    debug!("✅ Validator started in {:?}", start.elapsed());

    let lamports = expect!(ctx.fetch_ephem_account_balance(pubkey), validator);
    assert_eq!(lamports, 1_111_111, cleanup(&mut validator));
    debug!(
        "✅ Verified account {} has {} lamports after restore",
        pubkey, lamports
    );

    validator
}
