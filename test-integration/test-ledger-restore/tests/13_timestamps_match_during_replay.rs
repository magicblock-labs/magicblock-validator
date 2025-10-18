use std::{path::Path, process::Child};

use cleanass::assert_eq;
use integration_test_tools::{
    expect, loaded_accounts::LoadedAccounts, tmpdir::resolve_tmp_dir,
    validator::cleanup,
};
use log::*;
use magicblock_config::LedgerResumeStrategy;
use solana_sdk::{
    signature::{Keypair, Signature},
    signer::Signer,
};
use solana_transaction_status::UiTransactionEncoding;
use test_kit::init_logger;
use test_ledger_restore::{
    airdrop_and_delegate_accounts, setup_offline_validator,
    setup_validator_with_local_remote, transfer_lamports,
    wait_for_ledger_persist, SNAPSHOT_FREQUENCY, TMP_DIR_LEDGER,
};

// In this test we ensure that the timestamps of the blocks in the restored
// ledger match the timestamps of the blocks in the original ledger.

#[test]
fn test_restore_preserves_timestamps() {
    init_logger!();

    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (mut validator, slot, signature, block_time, _payer) =
        write(&ledger_path);
    validator.kill().unwrap();

    assert!(slot > SNAPSHOT_FREQUENCY);

    let mut validator = read(&ledger_path, signature, block_time);
    validator.kill().unwrap();
}

fn write(ledger_path: &Path) -> (Child, u64, Signature, i64, Keypair) {
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        true,
        false,
        &LoadedAccounts::default(),
    );

    // Wait to make sure we don't process transactions on slot 0
    expect!(ctx.wait_for_next_slot_ephem(), validator);

    // Airdrop and delegate two accounts
    let mut payers = airdrop_and_delegate_accounts(
        &ctx,
        &mut validator,
        &[2_000_000, 1_000_000],
    );
    let payer1 = payers.drain(0..1).next().unwrap();
    let payer2 = payers.drain(0..1).next().unwrap();
    debug!(
        "✅ Airdropped and delegated payers {} and {}",
        payer1.pubkey(),
        payer2.pubkey()
    );

    // Transfer lamports in ephem to create a transaction
    let signature = transfer_lamports(
        &ctx,
        &mut validator,
        &payer1,
        &payer2.pubkey(),
        111_111,
    );
    debug!("✅ Created transfer transaction {signature}");

    // Wait for the tx to be written to disk and slot to be finalized
    let slot = wait_for_ledger_persist(&ctx, &mut validator);
    debug!("✅ Ledger persisted at slot {slot}");

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
    debug!("✅ Retrieved block time {block_time} for signature");

    (validator, slot, signature, block_time, payer1)
}

fn read(ledger_path: &Path, signature: Signature, block_time: i64) -> Child {
    // Measure time
    let start = std::time::Instant::now();
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        None,
        LedgerResumeStrategy::Resume { replay: true },
        false,
    );
    debug!("✅ Validator started in {:?}", start.elapsed());

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
    debug!("✅ Retrieved restored block time {restored_block_time}");

    assert_eq!(restored_block_time, block_time, cleanup(&mut validator));
    debug!("✅ Verified timestamps match: original={block_time}, restored={restored_block_time}");

    validator
}
