use std::time::Duration;

use futures::StreamExt;
use setup::RpcTestEnv;
use solana_rpc_client_api::{
    config::{RpcTransactionLogsConfig, RpcTransactionLogsFilter},
    response::{ProcessedSignatureResult, RpcSignatureResult},
};
use test_kit::guinea;
use tokio::time::timeout;

mod setup;

/// Verifies `accountSubscribe` and `accountUnsubscribe` work correctly.
#[tokio::test]
async fn test_account_subscribe() {
    let env = RpcTestEnv::new().await;
    let account = env.create_account().pubkey;
    let amount = RpcTestEnv::TRANSFER_AMOUNT;

    // Subscribe to the account.
    let (mut stream, unsub) = env
        .pubsub
        .account_subscribe(&account, None)
        .await
        .expect("failed to subscribe to account");

    // Trigger an update by sending lamports to the account.
    env.transfer_lamports(account, amount).await;

    // Await the notification and verify its contents.
    let notification = timeout(Duration::from_millis(200), stream.next())
        .await
        .expect("timed out waiting for account notification")
        .expect("stream should not be closed");

    assert_eq!(
        notification.value.lamports,
        RpcTestEnv::INIT_ACCOUNT_BALANCE + amount
    );
    assert_eq!(notification.context.slot, env.latest_slot());

    // Unsubscribe and verify no more messages are received.
    unsub().await;
    let closed = stream.next().await.is_none();
    assert!(
        closed,
        "should not receive a notification after unsubscribing"
    );
}

/// Verifies `programSubscribe` receives notifications for account changes under a program.
#[tokio::test]
async fn test_program_subscribe() {
    let env = RpcTestEnv::new().await;

    // Subscribe to the test program.
    let (mut stream, unsub) = env
        .pubsub
        .program_subscribe(&guinea::ID, None)
        .await
        .expect("failed to subscribe to program");

    // Trigger an update by executing an instruction that modifies a program account.
    env.execute_transaction().await;

    // Await the notification and verify its contents.
    let notification = timeout(Duration::from_millis(200), stream.next())
        .await
        .expect("timed out waiting for program notification")
        .expect("stream should not be closed");

    assert_eq!(notification.value.account.data.decode().unwrap()[0], 42);

    unsub().await;
    let closed = stream.next().await.is_none();
    assert!(
        closed,
        "should not receive a notification after unsubscribing"
    );
}

/// Verifies `signatureSubscribe` for a successful transaction when subscribing *before* execution.
#[tokio::test]
async fn test_signature_subscribe_before_execution() {
    let env = RpcTestEnv::new().await;
    let transfer_tx = env.build_transfer_txn();
    let signature = transfer_tx.signatures[0];

    // Subscribe to the signature before sending the transaction.
    let (mut stream, unsub) = env
        .pubsub
        .signature_subscribe(&signature, None)
        .await
        .expect("failed to subscribe to signature");

    // Execute the transaction.
    env.execution
        .transaction_scheduler
        .execute(transfer_tx)
        .await
        .unwrap();

    // Await the notification and verify it indicates success.
    let notification = timeout(Duration::from_millis(200), stream.next())
        .await
        .expect("timed out waiting for signature notification")
        .expect("stream should not be closed")
        .value;

    assert!(
        matches!(
            notification,
            RpcSignatureResult::ProcessedSignature(ProcessedSignatureResult {
                err: None
            })
        ),
        "transaction should succeed"
    );
    unsub().await;

    // Verify it was a one-shot subscription by checking for more messages.
    let closed = stream.next().await.is_none();
    assert!(
        closed,
        "should not receive a notification after unsubscribing"
    );
}

/// Verifies `signatureSubscribe` for a successful transaction when subscribing *after* execution.
#[tokio::test]
async fn test_signature_subscribe_after_execution() {
    let env = RpcTestEnv::new().await;
    let signature = env.execute_transaction().await;

    // Subscribe to the signature *after* the transaction has been processed.
    // This tests the fast-path where the result is already cached.
    let (mut stream, _) = env
        .pubsub
        .signature_subscribe(&signature, None)
        .await
        .expect("failed to subscribe to signature");

    // Await the notification, which should be sent immediately.
    let notification = timeout(Duration::from_millis(200), stream.next())
        .await
        .expect("timed out waiting for signature notification")
        .expect("stream should not be closed")
        .value;

    assert!(
        matches!(
            notification,
            RpcSignatureResult::ProcessedSignature(ProcessedSignatureResult {
                err: None
            })
        ),
        "transaction should succeed"
    );
}

/// Verifies `signatureSubscribe` for a transaction that fails execution.
#[tokio::test]
async fn test_signature_subscribe_failure() {
    let env = RpcTestEnv::new().await;
    let failing_tx = env.build_failing_transfer_txn();
    let signature = failing_tx.signatures[0];

    let (mut stream, _) = env
        .pubsub
        .signature_subscribe(&signature, None)
        .await
        .expect("failed to subscribe to signature");

    env.execution
        .transaction_scheduler
        .schedule(failing_tx) // Use schedule for fire-and-forget
        .await
        .unwrap();

    let notification = timeout(Duration::from_millis(200), stream.next())
        .await
        .expect("timed out waiting for signature notification")
        .expect("stream should not be closed")
        .value;

    assert!(
        matches!(
            notification,
            RpcSignatureResult::ProcessedSignature(ProcessedSignatureResult {
                err: Some(_)
            })
        ),
        "transaction should have failed"
    );
}

/// Verifies `slotSubscribe` sends a notification for each new slot.
#[tokio::test]
async fn test_slot_subscribe() {
    let env = RpcTestEnv::new().await;
    let (mut stream, unsub) = env
        .pubsub
        .slot_subscribe()
        .await
        .expect("failed to subscribe to slots");
    let initial_slot = env.latest_slot();

    for i in 1..=3 {
        env.advance_slots(1);
        let notification = timeout(Duration::from_millis(200), stream.next())
            .await
            .expect("timed out waiting for slot notification")
            .expect("stream should not be closed");

        assert_eq!(notification.slot, initial_slot + i);
        assert_eq!(notification.parent, initial_slot + i - 1);
    }

    unsub().await;
    let closed = stream.next().await.is_none();
    assert!(
        closed,
        "should not receive a notification after unsubscribing"
    );
}

/// Verifies `logsSubscribe` with an `All` filter receives all transaction logs.
#[tokio::test]
async fn test_logs_subscribe_all() {
    let env = RpcTestEnv::new().await;

    let (mut stream, unsub) = env
        .pubsub
        .logs_subscribe(
            RpcTransactionLogsFilter::All,
            RpcTransactionLogsConfig { commitment: None },
        )
        .await
        .expect("failed to subscribe to all logs");

    let signature = env.execute_transaction().await;

    let notification = timeout(Duration::from_millis(200), stream.next())
        .await
        .expect("timed out waiting for log notification")
        .expect("stream should not be closed");

    assert_eq!(notification.value.signature, signature.to_string());
    assert!(notification.value.err.is_none());
    assert!(!notification.value.logs.is_empty());

    unsub().await;
    let closed = stream.next().await.is_none();
    assert!(
        closed,
        "should not receive a notification after unsubscribing"
    );
}

/// Verifies `logsSubscribe` with a `Mentions` filter receives the correct logs.
#[tokio::test]
async fn test_logs_subscribe_mentions() {
    let env = RpcTestEnv::new().await;

    let (mut stream, unsub) = env
        .pubsub
        .logs_subscribe(
            RpcTransactionLogsFilter::Mentions(vec![guinea::ID.to_string()]),
            RpcTransactionLogsConfig { commitment: None },
        )
        .await
        .expect("failed to subscribe to logs mentioning guinea program");

    // This transaction mentions the guinea program ID.
    let signature = env.execute_transaction().await;

    let notification = timeout(Duration::from_millis(200), stream.next())
        .await
        .expect("timed out waiting for log notification")
        .expect("stream should not be closed");

    assert_eq!(notification.value.signature, signature.to_string());
    assert!(notification.value.err.is_none());

    unsub().await;
    let closed = stream.next().await.is_none();
    assert!(
        closed,
        "should not receive a notification after unsubscribing"
    );
}
