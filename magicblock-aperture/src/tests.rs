use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};

use hyper::body::Bytes;
use magicblock_accounts_db::AccountsDb;
use solana_pubkey::Pubkey;
use test_kit::{
    guinea::{self, GuineaInstruction},
    AccountMeta, ExecutionTestEnv, Instruction, Signer,
};
use tokio::{
    sync::mpsc::{channel, Receiver},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

use crate::{
    encoder::{AccountEncoder, ProgramAccountEncoder, TransactionLogsEncoder},
    server::websocket::dispatch::WsConnectionChannel,
    state::{ChainlinkImpl, SharedState},
    utils::ProgramFilters,
    EventProcessor,
};

const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

/// A test helper to create a unique WebSocket connection channel pair.
fn ws_channel() -> (WsConnectionChannel, Receiver<Bytes>) {
    static CHAN_ID: AtomicU32 = AtomicU32::new(0);
    let id = CHAN_ID.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = channel(64);
    let tx = WsConnectionChannel { id, tx };
    (tx, rx)
}

fn chainlink(accounts_db: &Arc<AccountsDb>) -> ChainlinkImpl {
    ChainlinkImpl::try_new(
        accounts_db,
        None,
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        0,
    )
    .expect("Failed to create Chainlink")
}

mod event_processor {
    use super::*;
    use crate::state::NodeContext;

    /// Sets up a shared state and test environment for event processor tests.
    /// This initializes a validator backend, starts the event processor, and
    /// advances the slot to ensure a clean state.
    fn setup() -> (SharedState, ExecutionTestEnv) {
        let env = ExecutionTestEnv::new();
        env.advance_slot();
        let node_context = NodeContext {
            identity: env.get_payer().pubkey,
            ..Default::default()
        };
        let state = SharedState::new(
            node_context,
            env.accountsdb.clone(),
            env.ledger.clone(),
            Arc::new(chainlink(&env.accountsdb)),
        );
        let cancel = CancellationToken::new();
        EventProcessor::start(&state, &env.dispatch, 1, cancel);
        env.advance_slot();
        (state, env)
    }

    /// Awaits a message from a receiver with a timeout, panicking if no message
    /// arrives or if the message is empty.
    async fn assert_receives_update(rx: &mut Receiver<Bytes>, context: &str) {
        let update = timeout(Duration::from_millis(100), rx.recv())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out waiting for an event processor update for {}",
                    context
                )
            });

        let received_bytes =
            update.expect("subscription channel was closed unexpectedly");
        assert!(
            !received_bytes.is_empty(),
            "update from event processor for {} should not be empty",
            context
        );
    }

    /// Verifies that modifying an account triggers notifications for both
    /// a direct `accountSubscribe` and its parent `programSubscribe`.
    #[tokio::test]
    async fn test_account_update() {
        let (state, env) = setup();
        let acc = env
            .create_account_with_config(LAMPORTS_PER_SOL, 1, guinea::ID)
            .pubkey();
        let (tx, mut rx) = ws_channel();

        // Subscribe to both the specific account and the program that owns it.
        let _acc_sub = state
            .subscriptions
            .subscribe_to_account(acc, AccountEncoder::Base58, tx.clone())
            .await;
        let _prog_sub = state
            .subscriptions
            .subscribe_to_program(
                guinea::ID,
                ProgramAccountEncoder {
                    encoder: AccountEncoder::Base58,
                    filters: ProgramFilters::default(),
                },
                tx,
            )
            .await;

        // Execute a transaction that modifies the account.
        let ix = Instruction::new_with_bincode(
            guinea::ID,
            &GuineaInstruction::WriteByteToData(42),
            vec![AccountMeta::new(acc, false)],
        );
        env.execute_transaction(env.build_transaction(&[ix]))
            .await
            .unwrap();

        // Assert that both subscriptions received an update.
        assert_receives_update(&mut rx, "account subscription").await;
        assert_receives_update(&mut rx, "program subscription").await;
    }

    /// Verifies that executing a transaction triggers notifications for
    /// `signatureSubscribe` and the relevant `logsSubscribe` variants.
    #[tokio::test]
    async fn test_transaction_update() {
        let (state, env) = setup();
        let acc = env
            .create_account_with_config(LAMPORTS_PER_SOL, 42, guinea::ID)
            .pubkey();
        let (tx, mut rx) = ws_channel();

        let ix = Instruction::new_with_bincode(
            guinea::ID,
            &GuineaInstruction::PrintSizes,
            vec![AccountMeta::new_readonly(acc, false)],
        );
        let txn = env.build_transaction(&[ix]);

        // Subscribe to the signature, all logs, and logs mentioning the specific account.
        let _sig_sub = state
            .subscriptions
            .subscribe_to_signature(txn.signatures[0], tx.clone())
            .await;
        let _logs_all_sub = state
            .subscriptions
            .subscribe_to_logs(TransactionLogsEncoder::All, tx.clone());
        let _logs_mention_sub = state
            .subscriptions
            .subscribe_to_logs(TransactionLogsEncoder::Mentions(acc), tx);

        env.execute_transaction(txn).await.unwrap();

        // Assert that all three subscriptions received an update.
        assert_receives_update(&mut rx, "signature subscription").await;
        assert_receives_update(&mut rx, "all logs subscription").await;
        assert_receives_update(&mut rx, "logs mentions subscription").await;
    }

    /// Verifies that multiple `slotSubscribe` clients receive updates for every new slot.
    #[tokio::test]
    async fn test_block_update() {
        let (state, env) = setup();
        let (tx1, mut rx1) = ws_channel();
        let (tx2, mut rx2) = ws_channel();
        let _slot_sub1 = state.subscriptions.subscribe_to_slot(tx1);
        let _slot_sub2 = state.subscriptions.subscribe_to_slot(tx2);

        for i in 0..10 {
            // Test a sequence of slot advancements
            env.advance_slot();
            assert_receives_update(
                &mut rx1,
                &format!("slot update for sub1 #{}", i + 1),
            )
            .await;
            assert_receives_update(
                &mut rx2,
                &format!("slot update for sub2 #{}", i + 1),
            )
            .await;
        }
    }

    /// Verifies that multiple subscribers to the same resource (account/program) all receive notifications.
    #[tokio::test]
    async fn test_multisub() {
        let (state, env) = setup();

        // Test multiple subscriptions to the same ACCOUNT.
        let acc1 = env
            .create_account_with_config(LAMPORTS_PER_SOL, 1, guinea::ID)
            .pubkey();
        let (acc_tx1, mut acc_rx1) = ws_channel();
        let (acc_tx2, mut acc_rx2) = ws_channel();

        let _acc_sub1 = state
            .subscriptions
            .subscribe_to_account(acc1, AccountEncoder::Base58, acc_tx1)
            .await;
        let _acc_sub2 = state
            .subscriptions
            .subscribe_to_account(acc1, AccountEncoder::Base58, acc_tx2)
            .await;

        let ix1 = Instruction::new_with_bincode(
            guinea::ID,
            &GuineaInstruction::WriteByteToData(10),
            vec![AccountMeta::new(acc1, false)],
        );
        env.execute_transaction(env.build_transaction(&[ix1]))
            .await
            .unwrap();

        assert_receives_update(&mut acc_rx1, "first account subscriber").await;
        assert_receives_update(&mut acc_rx2, "second account subscriber").await;

        // Test multiple subscriptions to the same PROGRAM.
        let acc2 = env
            .create_account_with_config(LAMPORTS_PER_SOL, 1, guinea::ID)
            .pubkey();
        let (prog_tx1, mut prog_rx1) = ws_channel();
        let (prog_tx2, mut prog_rx2) = ws_channel();
        let prog_encoder = ProgramAccountEncoder {
            encoder: AccountEncoder::Base58,
            filters: ProgramFilters::default(),
        };

        let _prog_sub1 = state
            .subscriptions
            .subscribe_to_program(guinea::ID, prog_encoder.clone(), prog_tx1)
            .await;
        let _prog_sub2 = state
            .subscriptions
            .subscribe_to_program(guinea::ID, prog_encoder, prog_tx2)
            .await;

        let ix2 = Instruction::new_with_bincode(
            guinea::ID,
            &GuineaInstruction::WriteByteToData(20),
            vec![AccountMeta::new(acc2, false)],
        );
        env.execute_transaction(env.build_transaction(&[ix2]))
            .await
            .unwrap();

        assert_receives_update(&mut prog_rx1, "first program subscriber").await;
        assert_receives_update(&mut prog_rx2, "second program subscriber")
            .await;
    }

    /// Verifies that multiple subscribers to `logs` subscriptions all receive notifications.
    #[tokio::test]
    async fn test_logs_multisub() {
        let (state, env) = setup();
        let mentioned_acc = Pubkey::new_unique();

        // Multiple subscriptions to `logs(All)`.
        let (all_tx1, mut all_rx1) = ws_channel();
        let (all_tx2, mut all_rx2) = ws_channel();
        let _all_sub1 = state
            .subscriptions
            .subscribe_to_logs(TransactionLogsEncoder::All, all_tx1);
        let _all_sub2 = state
            .subscriptions
            .subscribe_to_logs(TransactionLogsEncoder::All, all_tx2);

        // Multiple subscriptions to `logs(Mentions)`.
        let (mention_tx1, mut mention_rx1) = ws_channel();
        let (mention_tx2, mut mention_rx2) = ws_channel();
        let _mention_sub1 = state.subscriptions.subscribe_to_logs(
            TransactionLogsEncoder::Mentions(mentioned_acc),
            mention_tx1,
        );
        let _mention_sub2 = state.subscriptions.subscribe_to_logs(
            TransactionLogsEncoder::Mentions(mentioned_acc),
            mention_tx2,
        );

        // Execute a transaction that mentions the target account.
        let ix = Instruction::new_with_bincode(
            guinea::ID,
            &GuineaInstruction::PrintSizes,
            vec![AccountMeta::new_readonly(mentioned_acc, false)],
        );
        env.execute_transaction(env.build_transaction(&[ix]))
            .await
            .unwrap();

        // Assert all four subscriptions received the update.
        assert_receives_update(&mut all_rx1, "first 'all logs' subscriber")
            .await;
        assert_receives_update(&mut all_rx2, "second 'all logs' subscriber")
            .await;
        assert_receives_update(&mut mention_rx1, "first 'mentions' subscriber")
            .await;
        assert_receives_update(
            &mut mention_rx2,
            "second 'mentions' subscriber",
        )
        .await;
    }
}

/// Unit tests for the `SubscriptionsDb` RAII-based automatic unsubscription mechanism.
mod subscriptions_db {
    use super::*;
    use crate::state::subscriptions::SubscriptionsDb;

    /// Verifies that dropping a subscription handle correctly removes the subscription
    /// from the central database for all subscription types.
    #[tokio::test]
    async fn test_auto_unsubscription() {
        // A local helper to test the RAII-based unsubscription. It asserts a
        // condition before and after a handle is dropped to verify cleanup.
        async fn check_unsubscription<H, C1, C2>(
            handle: H,
            check_before: C1,
            check_after: C2,
        ) where
            C1: FnOnce(),
            C2: FnOnce(),
        {
            // 1. Assert that the subscription was registered successfully.
            check_before();
            // 2. Drop the handle, which should trigger the unsubscription logic.
            drop(handle);
            // 3. Yield to the Tokio runtime to allow the background cleanup task to execute.
            tokio::task::yield_now().await;
            // 4. Assert that the subscription was removed from the database.
            check_after();
        }

        let db = SubscriptionsDb::default();
        let (tx, _) = ws_channel();

        // Test account unsubscription.
        let account_handle = db
            .subscribe_to_account(
                Pubkey::new_unique(),
                AccountEncoder::Base58,
                tx.clone(),
            )
            .await;
        check_unsubscription(
            account_handle,
            || {
                assert_eq!(
                    db.accounts.len(),
                    1,
                    "Account sub should be registered"
                )
            },
            || assert!(db.accounts.is_empty(), "Account sub should be removed"),
        )
        .await;

        // Test program unsubscription.
        let program_handle = db
            .subscribe_to_program(
                guinea::ID,
                ProgramAccountEncoder {
                    encoder: AccountEncoder::Base58,
                    filters: ProgramFilters::default(),
                },
                tx.clone(),
            )
            .await;
        check_unsubscription(
            program_handle,
            || {
                assert_eq!(
                    db.programs.len(),
                    1,
                    "Program sub should be registered"
                )
            },
            || assert!(db.programs.is_empty(), "Program sub should be removed"),
        )
        .await;

        // Test logs unsubscription.
        {
            let logs_all =
                db.subscribe_to_logs(TransactionLogsEncoder::All, tx.clone());
            let logs_mention = db.subscribe_to_logs(
                TransactionLogsEncoder::Mentions(Pubkey::new_unique()),
                tx.clone(),
            );
            assert_eq!(db.logs.read().count(), 2, "Two log subs should exist");
            drop((logs_all, logs_mention));
            tokio::task::yield_now().await;
            assert_eq!(db.logs.read().count(), 0, "Log subs should be removed");
        }

        // Test slot unsubscription.
        let slot_handle = db.subscribe_to_slot(tx);
        check_unsubscription(
            slot_handle,
            || {
                assert_eq!(
                    db.slot.read().count(),
                    1,
                    "Slot sub should be registered"
                )
            },
            || {
                assert_eq!(
                    db.slot.read().count(),
                    0,
                    "Slot sub should be removed"
                )
            },
        )
        .await;
    }
}
