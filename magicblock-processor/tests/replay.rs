use std::time::Duration;

use guinea::GuineaInstruction;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::link::transactions::SanitizeableTransaction;
use solana_account::ReadableAccount;
use solana_message::{v0, v1, Message, VersionedMessage};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::{
    sanitized::SanitizedTransaction, versioned::VersionedTransaction,
};
use solana_transaction_status::TransactionStatusMeta;
use test_kit::ExecutionTestEnv;

const ACCOUNTS_COUNT: usize = 8;
const TIMEOUT: Duration = Duration::from_millis(100);

/// Sets up a replay scenario in Replica mode:
/// - Transaction is written directly to Ledger (no execution)
/// - AccountsDb remains in pre-transaction state
fn setup_replay_scenario_replica(
    env: &ExecutionTestEnv,
    ix: GuineaInstruction,
    version: u8,
) -> (SanitizedTransaction, Vec<Pubkey>) {
    // 1. Create Accounts
    let accounts: Vec<_> = (0..ACCOUNTS_COUNT)
        .map(|_| {
            env.create_account_with_config(LAMPORTS_PER_SOL, 128, guinea::ID)
        })
        .collect();

    let metas = accounts
        .iter()
        .map(|a| AccountMeta::new(a.pubkey(), false))
        .collect();
    let pubkeys: Vec<_> = accounts.iter().map(|a| a.pubkey()).collect();

    // 2. Build Transaction
    let ix = Instruction::new_with_bincode(guinea::ID, &ix, metas);
    let payer = &env.payers[0];
    let hash = env.ledger.latest_blockhash();
    let message = match version {
        0 => VersionedMessage::Legacy(Message::new_with_blockhash(
            &[ix],
            Some(&payer.pubkey()),
            &hash,
        )),
        1 => VersionedMessage::V0(
            v0::Message::try_compile(&payer.pubkey(), &[ix], &[], hash)
                .unwrap(),
        ),
        _ => VersionedMessage::V1(
            v1::Message::try_compile_with_config(
                &payer.pubkey(),
                &[ix],
                hash,
                v1::TransactionConfig::default()
                    .with_compute_unit_limit(100_000)
                    .with_loaded_accounts_data_size_limit(1_000_000),
            )
            .unwrap(),
        ),
    };
    // Legacy builder rotates payers; all three versions must use the signed payer.
    let signer = env
        .payers
        .iter()
        .find(|key| key.pubkey() == message.static_account_keys()[0])
        .unwrap();
    let txn = VersionedTransaction::try_new(message, &[signer]).unwrap();
    let sanitized = txn.sanitize(true).unwrap();
    let sig = *sanitized.signature();

    // 3. Write transaction to ledger directly (without executing)
    // This simulates a transaction that was recorded by the primary
    let meta = TransactionStatusMeta {
        fee: 0,
        pre_balances: pubkeys.iter().map(|_| LAMPORTS_PER_SOL).collect(),
        post_balances: pubkeys.iter().map(|_| LAMPORTS_PER_SOL).collect(),
        status: Ok(()),
        ..Default::default()
    };
    let versioned = sanitized.to_versioned_transaction();
    let encoded = wincode::serialize(&versioned).unwrap();
    let locks = sanitized.get_account_locks_unchecked();
    env.ledger
        .write_transaction(
            sig,
            env.ledger.latest_block().load().slot,
            u32::from(version), // ordered mixed-version ledger records
            locks.writable,
            locks.readonly,
            &encoded,
            meta,
        )
        .expect("Failed to write transaction to ledger");

    // 4. Verify accounts are still in pre-transaction state
    for pubkey in &pubkeys {
        let account = env.accountsdb.get_account(pubkey).unwrap();
        assert_eq!(
            account.data()[0],
            0,
            "Account should be in pre-tx state before replay"
        );
    }

    let restored = env
        .ledger
        .read_transaction((sig, env.ledger.latest_block().load().slot))
        .unwrap()
        .unwrap();
    assert_eq!(restored, versioned);
    (restored.sanitize(true).unwrap(), pubkeys)
}

#[tokio::test]
pub async fn test_replay_state_transition() {
    // Run in Replica mode (scheduler starts in Replica, no mode switch)
    let env = ExecutionTestEnv::new_replica_mode(1, false);
    env.yield_to_scheduler().await;

    let payer_balance = env
        .accountsdb
        .get_account(&env.payers[0].pubkey())
        .unwrap()
        .lamports();
    for version in 0..3 {
        let (txn, pubkeys) = setup_replay_scenario_replica(
            &env,
            GuineaInstruction::WriteByteToData(42),
            version,
        );

        // 1. Verify Pre-Replay State
        for pubkey in &pubkeys {
            let account = env.accountsdb.get_account(pubkey).unwrap();
            assert_eq!(
                account.data()[0],
                0,
                "Account should be in pre-tx state"
            );
        }

        // 2. Perform Replay (persist=false: no status notifications)
        assert!(env.replay_transaction(false, txn).await.is_ok());

        // 3. Verify No Side Effects (Notifications)
        assert!(
            env.dispatch
                .transaction_status
                .recv_timeout(TIMEOUT)
                .is_err(),
            "Replay should NOT broadcast status updates"
        );
        assert!(
            env.dispatch.account_update.try_recv().is_err(),
            "Replay should NOT broadcast account updates"
        );

        // 4. Verify Post-Replay State (Applied)
        for pubkey in &pubkeys {
            let account = env.accountsdb.get_account(pubkey).unwrap();
            assert_eq!(
                account.data()[0],
                42,
                "Replay should update account state"
            );
        }
        assert_eq!(
            env.accountsdb
                .get_account(&env.payers[0].pubkey())
                .unwrap()
                .lamports(),
            payer_balance
        );
    }
}
