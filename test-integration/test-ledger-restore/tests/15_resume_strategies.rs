use std::{path::Path, process::Child};

use cleanass::{assert, assert_eq};
use integration_test_tools::{
    expect, tmpdir::resolve_tmp_dir, validator::cleanup,
};
use log::*;
use magicblock_config::LedgerResumeStrategy;
use solana_sdk::{
    signature::{Keypair, Signature},
    signer::Signer,
};
use test_kit::init_logger;
use test_ledger_restore::{
    airdrop_and_delegate_accounts, setup_validator_with_local_remote,
    setup_validator_with_local_remote_and_resume_strategy, transfer_lamports,
    wait_for_ledger_persist, wait_for_next_slot_after_account_snapshot,
    SNAPSHOT_FREQUENCY, TMP_DIR_LEDGER,
};

#[test]
fn test_restore_ledger_resume_strategy_reset_all() {
    init_logger!();

    test_resume_strategy(LedgerResumeStrategy::Reset {
        slot: 1000,
        keep_accounts: false,
    });
}

#[test]
fn test_restore_ledger_resume_strategy_reset_keep_accounts() {
    init_logger!();
    test_resume_strategy(LedgerResumeStrategy::Reset {
        slot: 1000,
        keep_accounts: true,
    });
}

#[test]
fn test_restore_ledger_resume_strategy_resume_with_replay() {
    init_logger!();
    test_resume_strategy(LedgerResumeStrategy::Resume { replay: true });
}

#[test]
fn test_restore_ledger_resume_strategy_resume_without_replay() {
    init_logger!();
    test_resume_strategy(LedgerResumeStrategy::Resume { replay: false });
}

pub fn test_resume_strategy(strategy: LedgerResumeStrategy) {
    let (_tmpdir, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let mut kp = Keypair::new();

    let (mut validator, slot, signature) = write(&ledger_path, &mut kp);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &kp, &signature, slot, strategy);
    validator.kill().unwrap();
}

pub fn write(ledger_path: &Path, kp: &mut Keypair) -> (Child, u64, Signature) {
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        true,
        true,
        &Default::default(),
    );

    // Wait slot 1 otherwise we might be unable to fetch the transaction status
    expect!(ctx.wait_for_next_slot_ephem(), validator);
    debug!("✅ Validator started and advanced to slot 1");

    // Airdrop and delegate the keypair
    let mut keypairs =
        airdrop_and_delegate_accounts(&ctx, &mut validator, &[1_111_111]);
    *kp = keypairs.drain(0..1).next().unwrap();
    debug!("✅ Airdropped and delegated keypair {}", kp.pubkey());

    let lamports =
        expect!(ctx.fetch_ephem_account_balance(&kp.pubkey()), validator);
    assert_eq!(lamports, 1_111_111, cleanup(&mut validator));
    debug!(
        "✅ Verified balance of {} lamports for {}",
        lamports,
        kp.pubkey()
    );

    // Wait for the next snapshot
    // We wait for one slot after the snapshot but the restarting validator will be at the previous slot
    let slot = wait_for_next_slot_after_account_snapshot(
        &ctx,
        &mut validator,
        SNAPSHOT_FREQUENCY,
    ) - 1;
    debug!("✅ Waited for snapshot at slot {}", slot);

    // Create another delegated account to transfer to
    let mut transfer_to_keypairs =
        airdrop_and_delegate_accounts(&ctx, &mut validator, &[1_000_000]);
    let transfer_to = transfer_to_keypairs.drain(0..1).next().unwrap();
    debug!(
        "✅ Created transfer target account {}",
        transfer_to.pubkey()
    );

    // Perform a transfer to create a real transaction signature
    let signature =
        transfer_lamports(&ctx, &mut validator, kp, &transfer_to.pubkey(), 100);
    debug!("✅ Created transfer transaction {}", signature);

    let to_lamports = expect!(
        ctx.fetch_ephem_account_balance(&transfer_to.pubkey()),
        validator
    );
    assert_eq!(to_lamports, 1_000_100, cleanup(&mut validator));
    debug!(
        "✅ Verified balance of {} lamports for receiving account {}",
        to_lamports,
        transfer_to.pubkey()
    );

    let from_lamports =
        expect!(ctx.fetch_ephem_account_balance(&kp.pubkey()), validator);
    assert_eq!(from_lamports, 1_111_011, cleanup(&mut validator));
    debug!(
        "✅ Verified balance of {} lamports for sending account {}",
        from_lamports,
        kp.pubkey()
    );

    // Wait more to be sure the ledger is persisted
    wait_for_ledger_persist(&ctx, &mut validator);
    debug!("✅ Ledger persisted");

    (validator, slot, signature)
}

pub fn read(
    ledger_path: &Path,
    kp: &Keypair,
    signature: &Signature,
    slot: u64,
    strategy: LedgerResumeStrategy,
) -> Child {
    debug!("✅ Reading ledger with strategy: {:?}", strategy);

    let (_, mut validator, ctx) =
        setup_validator_with_local_remote_and_resume_strategy(
            ledger_path,
            None,
            strategy.clone(),
            false,
            &Default::default(),
        );

    let validator_slot = expect!(ctx.get_slot_ephem(), validator);
    debug!("✅ Validator restarted at slot {}", validator_slot);

    // For Resume strategy, verify we're at or beyond the saved slot
    // For Reset strategy, we just continue from where we were
    let target_slot = match strategy {
        LedgerResumeStrategy::Resume { .. } => slot,
        LedgerResumeStrategy::Reset { .. } => 0,
    };
    assert!(
        validator_slot >= target_slot,
        cleanup(&mut validator),
        "{:?}: {} < {}",
        strategy,
        validator_slot,
        target_slot
    );
    debug!(
        "✅ Verified slot {} >= target slot {}",
        validator_slot, target_slot
    );

    // In ephemeral mode with delegated accounts, they will always be cloned from chain
    // For Resume strategies, the transfer will be replayed, reducing balance by 100
    // For Reset strategies, accounts are cloned fresh from chain with original balance
    let lamports =
        expect!(ctx.fetch_ephem_account_balance(&kp.pubkey()), validator);
    use LedgerResumeStrategy::*;
    let expected_lamports = match strategy {
        Resume { .. } => 1_111_011, // 1_111_111 - 100 (transfer)
        Reset { keep_accounts, .. } if keep_accounts => 1_111_011, // 1_111_111 - 100 (transfer)
        Reset { .. } => 1_111_111, // Fresh clone from chain
    };
    assert_eq!(
        lamports, expected_lamports,
        cleanup(&mut validator),
        "{:?}: Account balance should reflect strategy", strategy
    );
    debug!(
        "✅ Verified balance {} lamports for {} (expected: {})",
        lamports,
        kp.pubkey(),
        expected_lamports
    );

    // Transaction should not be found if we're removing the ledger
    let tx_result = ctx.get_transaction_ephem(signature);
    let tx_not_found = tx_result.is_err();
    assert!(
        tx_not_found == strategy.is_removing_ledger(),
        cleanup(&mut validator),
        "{:?} (removing ledger: {}, tx_not_found: {})",
        strategy,
        strategy.is_removing_ledger(),
        tx_not_found
    );
    debug!(
        "✅ Verified transaction state (removing ledger: {}, tx_not_found: {})",
        strategy.is_removing_ledger(),
        tx_not_found
    );

    validator
}
