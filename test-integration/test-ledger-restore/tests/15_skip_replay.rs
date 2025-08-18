use cleanass::{assert, assert_eq};
use magicblock_config::LedgerResumeStrategy;
use solana_transaction_status::UiTransactionEncoding;
use std::{path::Path, process::Child};

use integration_test_tools::{
    expect, tmpdir::resolve_tmp_dir, validator::cleanup,
};
use solana_sdk::{
    signature::{Keypair, Signature},
    signer::Signer,
};
use test_ledger_restore::{
    setup_offline_validator, wait_for_snapshot, TMP_DIR_LEDGER,
};

const SNAPSHOT_FREQUENCY: u64 = 2;

// In this test we ensure that we can optionally skip the replay of the ledger
// when restoring, restarting at the last slot.
#[test]
fn restore_ledger_skip_replay() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let keypairs = (0..10).map(|_| Keypair::new()).collect::<Vec<_>>();

    // Make some transactions
    let (mut validator, slot, signatures) = write(&ledger_path, &keypairs);
    validator.kill().unwrap();

    // Check that we're at the last slot and that the state is still there
    let mut validator = read(&ledger_path, &keypairs, &signatures, slot);
    validator.kill().unwrap();
}

fn write(
    ledger_path: &Path,
    keypairs: &[Keypair],
) -> (Child, u64, Vec<Signature>) {
    let millis_per_slot = 100;
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        Some(millis_per_slot),
        LedgerResumeStrategy::Reset,
        false,
    );

    let mut signatures = Vec::with_capacity(keypairs.len());
    for pubkey in keypairs.iter().map(|kp| kp.pubkey()) {
        let signature =
            expect!(ctx.airdrop_ephem(&pubkey, 1_111_111), validator);
        signatures.push(signature);

        let lamports =
            expect!(ctx.fetch_ephem_account_balance(&pubkey), validator);
        assert_eq!(lamports, 1_111_111, cleanup(&mut validator));
    }

    // Wait for the txs to be written to disk and save the actual snapshot slot
    let slot = wait_for_snapshot(&mut validator, SNAPSHOT_FREQUENCY) - 1;

    (validator, slot, signatures)
}

fn read(
    ledger_path: &Path,
    keypairs: &[Keypair],
    signatures: &[Signature],
    slot: u64,
) -> Child {
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        None,
        LedgerResumeStrategy::ResumeOnly,
        false,
    );

    // Current slot of the new validator should be at least the last slot of the previous validator
    let validator_slot = expect!(ctx.get_slot_ephem(), validator);
    assert!(validator_slot >= slot, cleanup(&mut validator));

    // Transactions should exist even without replay
    for (kp, signature) in keypairs.iter().zip(signatures) {
        // The state remains the same
        let lamports =
            expect!(ctx.fetch_ephem_account_balance(&kp.pubkey()), validator);
        assert_eq!(lamports, 1_111_111, cleanup(&mut validator));

        // Past transactions are lost
        assert!(
            ctx.try_ephem_client()
                .and_then(|client| client
                    .get_transaction(signature, UiTransactionEncoding::Base58)
                    .map_err(|e| anyhow::anyhow!("{}", e)))
                .is_err(),
            cleanup(&mut validator)
        );
    }

    validator
}
