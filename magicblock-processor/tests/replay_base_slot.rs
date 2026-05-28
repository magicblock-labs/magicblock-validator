// Regression test for: replay base slot must be accountsdb_slot + 1, not accountsdb_slot.
//
// The bug: process_ledger was called with full_process_starting_slot = accountsdb_slot
// (inclusive).  Because accountsdb_slot is the last slot whose effects are already in
// AccountsDb, this caused every successful transaction in that slot to be applied a
// second time.
//
// The fix: start replay at accountsdb_slot + 1 so the last persisted slot is never re-run.
//
// Test strategy:
//   1. Set up an account C with data[0] = 0 in AccountsDb.
//   2. Build a non-idempotent Increment transaction T1 targeting C.
//   3. Write T1 to the ledger at slot S (marked successful) but do NOT execute it yet.
//   4. Apply T1 to AccountsDb via replay_transaction — AccountsDb is now post-T1 (data[0] = 1)
//      and accountsdb_slot = S, exactly as after a clean shutdown.
//   5. Inject an empty block header at S+1 directly into the ledger WITHOUT calling
//      advance_slot() — ledger_slot is now S+1 while accountsdb_slot stays at S.
//   6. Call process_ledger with the correct starting slot (S+1): T1 must NOT be re-applied,
//      so data[0] stays 1.
//   7. A second test demonstrates the bug: process_ledger starting at S (the old code)
//      re-applies T1 → data[0] becomes 2.

use guinea::GuineaInstruction;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::link::transactions::SanitizeableTransaction;
use magicblock_ledger::{
    blockstore_processor::process_ledger, LatestBlockInner,
};
use solana_account::ReadableAccount;
use solana_keypair::Keypair;
use solana_program::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_signer::Signer;
use solana_transaction::sanitized::SanitizedTransaction;
use solana_transaction_status::TransactionStatusMeta;
use test_kit::ExecutionTestEnv;

async fn setup(env: &ExecutionTestEnv) -> Keypair {
    env.yield_to_scheduler().await;
    let kp = env.create_account_with_config(LAMPORTS_PER_SOL, 128, guinea::ID);
    assert_eq!(
        env.accountsdb.get_account(&kp.pubkey()).unwrap().data()[0],
        0,
        "account data must start at 0"
    );
    kp
}

fn build_increment_tx(
    env: &ExecutionTestEnv,
    kp: &Keypair,
) -> SanitizedTransaction {
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::Increment,
        vec![AccountMeta::new(kp.pubkey(), false)],
    );
    env.build_transaction(&[ix]).sanitize(true).unwrap()
}

fn write_tx_to_ledger(
    env: &ExecutionTestEnv,
    sanitized: &SanitizedTransaction,
    slot: u64,
) {
    let sig = *sanitized.signature();
    let meta = TransactionStatusMeta {
        fee: 0,
        pre_balances: vec![LAMPORTS_PER_SOL],
        post_balances: vec![LAMPORTS_PER_SOL],
        status: Ok(()),
        ..Default::default()
    };
    let versioned = sanitized.to_versioned_transaction();
    let encoded = bincode::serialize(&versioned).unwrap();
    let locks = sanitized.get_account_locks_unchecked();
    env.ledger
        .write_transaction(
            sig,
            slot,
            0,
            locks.writable,
            locks.readonly,
            &encoded,
            meta,
        )
        .expect("failed to write transaction to ledger");
}

fn inject_empty_block(env: &ExecutionTestEnv, slot: u64) {
    let latest = env.ledger.latest_block().load();
    let time = latest.clock.unix_timestamp;
    drop(latest);
    env.ledger
        .write_block(LatestBlockInner::new(slot, Hash::new_unique(), time + 1))
        .expect("failed to write injected block");
}

/// Correct behavior: process_ledger starting at accountsdb_slot + 1 does not re-apply T1.
#[tokio::test]
async fn test_replay_starts_after_accountsdb_slot() {
    let env = ExecutionTestEnv::new_replica_mode(1, false);
    let kp = setup(&env).await;

    let slot_s = env.ledger.latest_block().load().slot;

    let sanitized = build_increment_tx(&env, &kp);
    write_tx_to_ledger(&env, &sanitized, slot_s);

    // Apply T1 so AccountsDb reflects post-T1 state (data[0] = 1).
    env.replay_transaction(false, sanitized).await.unwrap();
    assert_eq!(
        env.accountsdb.get_account(&kp.pubkey()).unwrap().data()[0],
        1,
        "T1 must be applied to AccountsDb before replay test"
    );
    assert_eq!(env.accountsdb.slot(), slot_s);

    // Inject empty block at S+1 without advancing accountsdb_slot.
    inject_empty_block(&env, slot_s + 1);

    // Fix: replay starts at S+1, T1 at slot S is not re-applied.
    process_ledger(
        &env.ledger,
        slot_s + 1,
        env.transaction_scheduler.clone(),
        0,
    )
    .await
    .unwrap();

    assert_eq!(
        env.accountsdb.get_account(&kp.pubkey()).unwrap().data()[0],
        1,
        "data[0] must remain 1 — T1 must not be replayed again"
    );
}

/// Bug demonstration: process_ledger starting at accountsdb_slot re-applies T1.
#[tokio::test]
async fn test_replay_from_accountsdb_slot_double_applies() {
    let env = ExecutionTestEnv::new_replica_mode(1, false);
    let kp = setup(&env).await;

    let slot_s = env.ledger.latest_block().load().slot;

    let sanitized = build_increment_tx(&env, &kp);
    write_tx_to_ledger(&env, &sanitized, slot_s);

    // Apply T1 (data[0] = 1).
    env.replay_transaction(false, sanitized).await.unwrap();
    assert_eq!(
        env.accountsdb.get_account(&kp.pubkey()).unwrap().data()[0],
        1,
        "T1 must be applied to AccountsDb before replay test"
    );

    inject_empty_block(&env, slot_s + 1);

    // Bug: replay starts at S (inclusive), so T1 is applied a second time.
    process_ledger(&env.ledger, slot_s, env.transaction_scheduler.clone(), 0)
        .await
        .unwrap();

    assert_eq!(
        env.accountsdb.get_account(&kp.pubkey()).unwrap().data()[0],
        2,
        "data[0] must be 2 — T1 was replayed a second time (double-apply bug)"
    );
}
