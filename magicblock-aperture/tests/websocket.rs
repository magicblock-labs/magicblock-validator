use std::time::Duration;

use futures::StreamExt;
use setup::{PROGRAM_ID, RpcTestEnv};
use solana_account::AccountMode;
use solana_rpc_client_api::{
    config::{RpcTransactionLogsConfig, RpcTransactionLogsFilter},
    response::{ProcessedSignatureResult, RpcSignatureResult},
};
use tokio::time::timeout;

mod setup;

/// Verifies `accountSubscribe` and `accountUnsubscribe` work correctly.
#[tokio::test]
async fn test_account_subscribe() {
    let env = RpcTestEnv::new().await;
    let account = env.engine.store_v42(0, AccountMode::Ephemeral);
    let balance = env.engine.load_v42_lamports(account).unwrap();
    let amount = RpcTestEnv::TRANSFER_AMOUNT;

    // Subscribe to the account.
    let (mut stream, unsub) = env
        .pubsub
        .account_subscribe(&account, None)
        .await
        .expect("failed to subscribe to account");

    let sender = env.engine.store_v42(0, AccountMode::Ephemeral);
    env.engine
        .execute(&[v42_calculator_interface::builder::transfer(
            sender, account, amount,
        )])
        .await
        .expect("transfer succeeds");

    // Await the notification and verify its contents.
    let notification = timeout(Duration::from_millis(200), stream.next())
        .await
        .expect("timed out waiting for account notification")
        .expect("stream should not be closed");

    assert_eq!(notification.value.lamports, balance + amount);
    assert_eq!(notification.context.slot, env.engine.blocks().latest().slot);

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
        .program_subscribe(&PROGRAM_ID, None)
        .await
        .expect("failed to subscribe to program");

    // Trigger an update by executing an instruction that modifies a program account.
    env.execute_write().await;

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
    let sender = env.engine.store_v42(0, AccountMode::Ephemeral);
    let recipient = env.engine.store_v42(0, AccountMode::Ephemeral);
    let (signature, view) = env.engine.signed_view(
        None,
        v42_calculator_interface::builder::transfer(
            sender,
            recipient,
            RpcTestEnv::TRANSFER_AMOUNT,
        ),
    );

    // Subscribe to the signature before sending the transaction.
    let (mut stream, unsub) = env
        .pubsub
        .signature_subscribe(&signature, None)
        .await
        .expect("failed to subscribe to signature");

    env.engine
        .transaction(view)
        .expect("compose transaction view")
        .execute()
        .await
        .expect("engine available")
        .expect("transfer succeeds");

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
    let signature = env.execute_write().await;

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
    let sender = env.engine.store_v42(0, AccountMode::Ephemeral);
    let recipient = env.engine.store_v42(0, AccountMode::Ephemeral);
    let amount = env.engine.load_v42_lamports(sender).unwrap() + 1;
    let (signature, view) = env.engine.signed_view(
        None,
        v42_calculator_interface::builder::transfer(sender, recipient, amount),
    );

    let (mut stream, _) = env
        .pubsub
        .signature_subscribe(&signature, None)
        .await
        .expect("failed to subscribe to signature");

    env.engine
        .transaction(view)
        .expect("compose transaction view")
        .schedule()
        .await
        .expect("schedule failing transfer");

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
    let mut env = RpcTestEnv::new().await;
    let (mut stream, unsub) = env
        .pubsub
        .slot_subscribe()
        .await
        .expect("failed to subscribe to slots");
    // Each deterministic advance produces exactly one block, so the subscription
    // should deliver exactly one strictly-increasing slot notification per step.
    let mut last_slot: Option<u64> = None;
    for _ in 0..3 {
        env.engine.advance(1).await;
        let notification = timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("timed out waiting for slot notification")
            .expect("stream should not be closed");

        if let Some(prev) = last_slot {
            assert!(
                notification.slot > prev,
                "slot should advance: {prev} -> {}",
                notification.slot
            );
        }
        assert_eq!(notification.parent, notification.slot - 1);
        last_slot = Some(notification.slot);
    }

    unsub().await;
    let closed = stream.next().await.is_none();
    assert!(
        closed,
        "should not receive a notification after unsubscribing"
    );
}

/// The engine-backed `logsSubscribe` is mentions-only: the firehose `All`
/// filter is no longer supported and must be rejected.
#[tokio::test]
async fn test_logs_subscribe_all_rejected() {
    let env = RpcTestEnv::new().await;

    let result = env
        .pubsub
        .logs_subscribe(
            RpcTransactionLogsFilter::All,
            RpcTransactionLogsConfig { commitment: None },
        )
        .await;

    assert!(
        result.is_err(),
        "logsSubscribe(All) should be rejected; only Mentions is supported"
    );
}

/// Verifies `logsSubscribe` with a `Mentions` filter receives the correct logs.
#[tokio::test]
async fn test_logs_subscribe_mentions() {
    let env = RpcTestEnv::new().await;

    let (mut stream, unsub) = env
        .pubsub
        .logs_subscribe(
            RpcTransactionLogsFilter::Mentions(vec![PROGRAM_ID.to_string()]),
            RpcTransactionLogsConfig { commitment: None },
        )
        .await
        .expect("failed to subscribe to logs mentioning v42 program");

    // This transaction mentions the engine testkit's v42 program ID.
    let signature = env.execute_write().await;

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
