use cleanass::{assert, assert_eq};
use magicblock_config::LedgerResumeStrategy;
use std::{path::Path, process::Child};

use crate::{
    setup_offline_validator, wait_for_ledger_persist, wait_for_snapshot,
    TMP_DIR_LEDGER,
};
use integration_test_tools::{
    expect, tmpdir::resolve_tmp_dir, validator::cleanup,
};
use solana_sdk::{
    signature::{Keypair, Signature},
    signer::Signer,
};

// Snapshot frequency is set to 2 slots for the offline validator
const SNAPSHOT_FREQUENCY: u64 = 2;

pub fn test_resume_strategy(
    strategy: LedgerResumeStrategy,
    starting_slot: Option<u64>,
) {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let mut kp = Keypair::new();

    let (mut validator, slot, signature) = write(&ledger_path, &mut kp);
    validator.kill().unwrap();

    let mut validator =
        read(&ledger_path, &kp, &signature, slot, strategy, starting_slot);
    validator.kill().unwrap();
}

pub fn write(ledger_path: &Path, kp: &mut Keypair) -> (Child, u64, Signature) {
    let millis_per_slot = 100;
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        Some(millis_per_slot),
        LedgerResumeStrategy::Reset,
        None,
        false,
    );

    let signature =
        expect!(ctx.airdrop_ephem(&kp.pubkey(), 1_111_111), validator);

    let lamports =
        expect!(ctx.fetch_ephem_account_balance(&kp.pubkey()), validator);
    assert_eq!(lamports, 1_111_111, cleanup(&mut validator));

    // Wait for the next snapshot
    // We wait for one slot after the snapshot but the restarting validator will be at the previous slot
    let slot = wait_for_snapshot(&mut validator, SNAPSHOT_FREQUENCY) - 1;
    // Wait more to be sure the ledger is persisted
    wait_for_ledger_persist(&mut validator);

    (validator, slot, signature)
}

pub fn read(
    ledger_path: &Path,
    kp: &Keypair,
    signature: &Signature,
    slot: u64,
    strategy: LedgerResumeStrategy,
    starting_slot: Option<u64>,
) -> Child {
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        None,
        strategy.clone(),
        starting_slot,
        false,
    );

    if strategy.is_resuming() {
        let validator_slot = expect!(ctx.get_slot_ephem(), validator);
        assert!(
            validator_slot >= slot,
            cleanup(&mut validator),
            "{}: {} < {}",
            strategy,
            validator_slot,
            slot
        );
    } else {
        let validator_slot = expect!(ctx.get_slot_ephem(), validator);
        assert!(
            validator_slot >= starting_slot.unwrap_or(0),
            cleanup(&mut validator),
            "{}: {} < {}",
            strategy,
            validator_slot,
            starting_slot.unwrap_or(0)
        );
    }

    if strategy.is_removing_accountsdb() {
        let lamports =
            expect!(ctx.fetch_ephem_account_balance(&kp.pubkey()), validator);
        assert_eq!(lamports, 0, cleanup(&mut validator), "{}", strategy);
    } else {
        let lamports =
            expect!(ctx.fetch_ephem_account_balance(&kp.pubkey()), validator);
        assert_eq!(
            lamports, 1_111_111,
            cleanup(&mut validator),
            "{}", strategy
        );
    }

    if strategy.is_removing_ledger() {
        assert!(
            ctx.get_transaction_ephem(signature).is_err(),
            cleanup(&mut validator),
            "{}",
            strategy
        );
    } else {
        assert!(
            ctx.get_transaction_ephem(signature).is_ok(),
            cleanup(&mut validator),
            "{}",
            strategy
        );
    }

    validator
}
