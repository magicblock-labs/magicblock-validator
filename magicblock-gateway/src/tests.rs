use std::time::Duration;

use hyper::body::Bytes;
use tokio::sync::mpsc::{channel, Receiver};

use solana_pubkey::Pubkey;

use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use test_kit::{
    guinea::{self, GuineaInstruction},
    AccountMeta, ExecutionTestEnv, Instruction, Signer,
};

use crate::server::websocket::dispatch::WsConnectionChannel;
use crate::{
    encoder::{AccountEncoder, ProgramAccountEncoder, TransactionLogsEncoder},
    state::SharedState,
    utils::ProgramFilters,
    EventProcessor,
};

fn ws_channel() -> (WsConnectionChannel, Receiver<Bytes>) {
    let (tx, rx) = channel(64);
    let tx = WsConnectionChannel { id: 0, tx };
    (tx, rx)
}

mod event_processor {
    use super::*;

    fn setup() -> (SharedState, ExecutionTestEnv) {
        let identity = Pubkey::new_unique();
        let env = ExecutionTestEnv::new();
        env.advance_slot();
        let state = SharedState::new(
            identity,
            env.accountsdb.clone(),
            env.ledger.clone(),
            50,
        );
        let cancel = CancellationToken::new();
        EventProcessor::start(&state, &env.dispatch, 1, cancel);
        (state, env)
    }

    #[tokio::test]
    async fn test_account_update() {
        let (state, env) = setup();
        let acc = env.create_account_with_config(1, 1, guinea::ID).pubkey();
        let (tx, mut rx) = ws_channel();
        let _acc = state
            .subscriptions
            .subscribe_to_account(acc, AccountEncoder::Base58, tx.clone())
            .await;
        let _prog = state
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
        let ix = Instruction::new_with_bincode(
            guinea::ID,
            &GuineaInstruction::WriteByteToData(42),
            vec![AccountMeta::new(acc, false)],
        );
        let txn = env.build_transaction(&[ix]);
        env.execute_transaction(txn).await.unwrap();

        let update = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("failed to receive an event processor update for account");
        assert!(
            update.is_some(),
            "subscription for an account wasn't registered (channel closed)"
        );
        assert!(
            !update.unwrap().is_empty(),
            "update from event processor for account should not be empty"
        );
        let update = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("failed to receive an event processor update for program");
        assert!(
            !update.unwrap().is_empty(),
            "update from event processor for program should not be empty"
        );
    }

    #[tokio::test]
    async fn test_transaction_update() {
        let (state, env) = setup();
        let acc = env.create_account_with_config(1, 42, guinea::ID).pubkey();

        let (tx, mut rx) = ws_channel();
        let ix = Instruction::new_with_bincode(
            guinea::ID,
            &GuineaInstruction::PrintSizes,
            vec![AccountMeta::new_readonly(acc, false)],
        );
        let txn = env.build_transaction(&[ix]);
        let _sig = state
            .subscriptions
            .subscribe_to_signature(txn.signatures[0], tx.clone())
            .await;
        let _logs_all = state
            .subscriptions
            .subscribe_to_logs(TransactionLogsEncoder::All, tx.clone());
        let _logs_mention = state
            .subscriptions
            .subscribe_to_logs(TransactionLogsEncoder::Mentions(acc), tx);
        env.execute_transaction(txn)
            .await
            .expect("failed to execute read only transaction");

        let update =
            timeout(Duration::from_millis(100), rx.recv()).await.expect(
                "failed to receive an event processor update for signature",
            );
        assert!(
            update.is_some(),
            "subscription for an signature wasn't registered (channel closed)"
        );
        assert!(
            !update.unwrap().is_empty(),
            "update from event processor for signature should not be empty"
        );
        let update = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("failed to receive an event processor update for all logs");
        assert!(
            !update.unwrap().is_empty(),
            "update for all logs subscription shouldn't be empty"
        );
        let update = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("failed to receive an event processor update for logs with mentions");
        assert!(
            !update.unwrap().is_empty(),
            "update for logs with mentions subscription shouldn't be empty"
        );
    }
    #[tokio::test]
    async fn test_block_update() {
        let (state, env) = setup();
        let (tx, mut rx) = ws_channel();
        let _slot = state.subscriptions.subscribe_to_slot(tx);
        for _ in 0..42 {
            env.advance_slot();
            let update = timeout(Duration::from_millis(100), rx.recv())
                .await
                .expect("failed to receive an event processor update for slot");
            assert!(
                !update.unwrap().is_empty(),
                "update for slot subscription shouldn't be empty"
            );
        }
    }
}

mod subscriptions_db {
    use crate::state::subscriptions::SubscriptionsDb;

    use super::*;

    #[tokio::test]
    async fn test_auto_unsubscription() {
        let db = SubscriptionsDb::default();
        let (tx, _) = ws_channel();
        let mut handle = db
            .subscribe_to_account(
                Pubkey::new_unique(),
                AccountEncoder::Base58,
                tx.clone(),
            )
            .await;
        assert_eq!(
            db.accounts.len(), 1,
            "one account entry should have been inserted into subscriptions database"
        );
        drop(handle);
        // let the cleanup run
        tokio::task::yield_now().await;

        assert!(
            db.accounts.is_empty(),
            "accounts subscriptions database should not have entries after RAII handle drop"
        );
        handle = db
            .subscribe_to_program(
                guinea::ID,
                ProgramAccountEncoder {
                    encoder: AccountEncoder::Base58,
                    filters: ProgramFilters::default(),
                },
                tx.clone(),
            )
            .await;
        assert_eq!(
            db.programs.len(), 1,
            "one program entry should have been inserted into subscriptions database"
        );
        drop(handle);
        // let the cleanup run
        tokio::task::yield_now().await;

        assert!(
            db.programs.is_empty(),
            "program subscriptions database should not have entries after RAII handle drop"
        );
        let logs_all =
            db.subscribe_to_logs(TransactionLogsEncoder::All, tx.clone());
        let logs_mention = db.subscribe_to_logs(
            TransactionLogsEncoder::Mentions(Pubkey::new_unique()),
            tx.clone(),
        );
        assert_eq!(
            db.logs.read().count(),
            2,
            "two entries should have been inserted into logs subscriptions database"
        );
        drop(logs_all);
        drop(logs_mention);
        // let the cleanup tasks to run
        tokio::task::yield_now().await;

        assert_eq!(
            db.logs.read().count(), 0,
            "logs subscriptions database should not have entries after RAII handles drop"
        );

        handle = db.subscribe_to_slot(tx);

        assert_eq!(
            db.slot.read().count(),
            1,
            "an entry should have been inserted into slot subscriptions database"
        );
        drop(handle);
        // let the cleanup task to run
        tokio::task::yield_now().await;

        assert_eq!(
            db.slot.read().count(), 0,
            "slot subscriptions database should not have entries after RAII handles drop"
        );
    }
}
