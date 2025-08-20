use cleanass::{assert, assert_eq};
use magicblock_config::LedgerResumeStrategy;
use std::{path::Path, process::Child};

use integration_test_tools::{
    expect, tmpdir::resolve_tmp_dir, validator::cleanup,
};
use solana_sdk::{
    signature::{Keypair, Signature},
    signer::Signer,
};
use test_ledger_restore::{
    setup_offline_validator, wait_for_ledger_persist,
    wait_for_next_slot_after_account_snapshot, SNAPSHOT_FREQUENCY,
    TMP_DIR_LEDGER,
};

#[test]
fn restore_ledger_reset() {
    eprintln!("\n================\nReset\n================\n");
    test_resume_strategy(LedgerResumeStrategy::Reset {
        slot: 1000,
        keep_accounts: false,
    });
    eprintln!("\n================\nReset with accounts\n================\n");
    test_resume_strategy(LedgerResumeStrategy::Reset {
        slot: 1000,
        keep_accounts: false,
    });
    eprintln!("\n================\nResume\n================\n");
    test_resume_strategy(LedgerResumeStrategy::Resume { replay: true });
    eprintln!("\n================\nReplay\n================\n");
    test_resume_strategy(LedgerResumeStrategy::Resume { replay: false });
}

pub fn test_resume_strategy(strategy: LedgerResumeStrategy) {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let mut kp = Keypair::new();

    let (mut validator, slot, signature) = write(&ledger_path, &mut kp);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &kp, &signature, slot, strategy);
    validator.kill().unwrap();
}

pub fn write(ledger_path: &Path, kp: &mut Keypair) -> (Child, u64, Signature) {
    let millis_per_slot = 100;
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        Some(millis_per_slot),
        LedgerResumeStrategy::Reset {
            slot: 0,
            keep_accounts: false,
        },
        false,
    );

    // Wait slot 1 otherwise we might be unable to fetch the transaction status
    expect!(ctx.wait_for_next_slot_ephem(), validator);

    let signature =
        expect!(ctx.airdrop_ephem(&kp.pubkey(), 1_111_111), validator);

    let lamports =
        expect!(ctx.fetch_ephem_account_balance(&kp.pubkey()), validator);
    assert_eq!(lamports, 1_111_111, cleanup(&mut validator));

    // Wait for the next snapshot
    // We wait for one slot after the snapshot but the restarting validator will be at the previous slot
    let slot = wait_for_next_slot_after_account_snapshot(
        &mut validator,
        SNAPSHOT_FREQUENCY,
    ) - 1;
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
) -> Child {
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        None,
        strategy.clone(),
        false,
    );

    let validator_slot = expect!(ctx.get_slot_ephem(), validator);
    let target_slot = match strategy {
        LedgerResumeStrategy::Reset { slot, .. } => slot,
        LedgerResumeStrategy::Resume { .. } => slot,
    };
    assert!(
        validator_slot >= target_slot,
        cleanup(&mut validator),
        "{:?}: {} < {}",
        strategy,
        validator_slot,
        target_slot
    );

    let lamports =
        expect!(ctx.fetch_ephem_account_balance(&kp.pubkey()), validator);
    let target_lamports = if strategy.is_removing_accountsdb() {
        0
    } else {
        1_111_111
    };
    assert_eq!(
        lamports,
        target_lamports,
        cleanup(&mut validator),
        "{:?} (removing ADB: {})",
        strategy,
        strategy.is_removing_accountsdb()
    );

    assert!(
        ctx.get_transaction_ephem(signature).is_err()
            == strategy.is_removing_ledger(),
        cleanup(&mut validator),
        "{:?} (removing ledger: {})",
        strategy,
        strategy.is_removing_ledger()
    );

    validator
}
