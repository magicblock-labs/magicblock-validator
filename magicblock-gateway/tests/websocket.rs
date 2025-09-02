use std::time::Duration;

use futures::StreamExt;
use setup::RpcTestEnv;
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
        .unwrap();

    assert_eq!(
        notification.value.lamports,
        RpcTestEnv::INIT_ACCOUNT_BALANCE + amount
    );
    assert_eq!(notification.context.slot, env.latest_slot());

    // Unsubscribe from the account.
    unsub().await;

    // Trigger another update.
    env.transfer_lamports(account, amount).await;

    // Verify that no new notification is received after unsubscribing.
    assert!(
        stream.next().await.is_none(),
        "should not receive a notification after unsubscribing"
    );
}

/// Verifies `programSubscribe` receives notifications for account changes under a program.
#[tokio::test]
async fn test_program_subscribe() {
    let env = RpcTestEnv::new().await;

    // Subscribe to the program.
    let (mut stream, unsub) = env
        .pubsub
        .program_subscribe(&guinea::ID, None)
        .await
        .expect("failed to subscribe to program");

    // Trigger an update by executing an instruction that modifies the program account.
    env.execute_transaction().await;

    // Await the notification and verify its contents.
    let notification = timeout(Duration::from_millis(200), stream.next())
        .await
        .expect("timed out waiting for program notification")
        .unwrap();

    assert_eq!(notification.value.account.data.decode().unwrap()[0], 42);

    unsub().await;
    // Verify that no new notification is received after unsubscribing.
    assert!(
        stream.next().await.is_none(),
        "should not receive a notification after unsubscribing"
    );
}

// /// Verifies `signatureSubscribe` for a successful transaction.
// #[tokio::test]
// async fn test_signature_subscribe_success() {
//     let env = RpcTestEnv::new().await;
//     let transfer_tx = env.build_transfer_txn();
//     let signature = transfer_tx.signatures[0];

//     // Subscribe to the signature before sending the transaction.
//     let (mut stream, _) = env
//         .pubsub
//         .signature_subscribe(&signature, Some(CommitmentConfig::processed()))
//         .await
//         .expect("failed to subscribe to signature");

//     // Send the transaction.
//     env.execution
//         .transaction_scheduler
//         .schedule(transfer_tx)
//         .await
//         .unwrap();

//     // Await the notification.
//     let notification =
//         tokio::time::timeout(Duration::from_secs(2), stream.next())
//             .await
//             .expect("timed out waiting for signature notification")
//             .unwrap()
//             .unwrap();

//     // Verify the transaction was successful.
//     assert!(
//         notification.value.err.is_none(),
//         "transaction should succeed"
//     );

//     // Verify it was a one-shot subscription by checking for more messages.
//     let no_notification =
//         tokio::time::timeout(Duration::from_millis(50), stream.next()).await;
//     assert!(
//         no_notification.is_err() || no_notification.unwrap().is_none(),
//         "subscription should be one-shot"
//     );
// }

// /// Verifies `signatureSubscribe` for a transaction that fails execution.
// #[tokio::test]
// async fn test_signature_subscribe_failure() {
//     let env = RpcTestEnv::new().await;
//     let failing_tx = env.build_failing_transfer_txn();
//     let signature = failing_tx.signatures[0];

//     // Subscribe to the signature.
//     let (mut stream, _) = env
//         .pubsub
//         .signature_subscribe(&signature, Some(CommitmentConfig::processed()))
//         .await
//         .expect("failed to subscribe to signature");

//     // Send the failing transaction.
//     env.execution
//         .transaction_scheduler
//         .schedule(failing_tx)
//         .await
//         .unwrap();

//     // Await the notification.
//     let notification =
//         tokio::time::timeout(Duration::from_secs(2), stream.next())
//             .await
//             .expect("timed out waiting for signature notification")
//             .unwrap()
//             .unwrap();

//     // Verify the transaction failed.
//     assert!(notification.value.err.is_some(), "transaction should fail");
// }

// /// Verifies `slotSubscribe` sends a notification for each new slot.
// #[tokio::test]
// async fn test_slot_subscribe() {
//     let env = RpcTestEnv::new().await;

//     let (mut stream, unsub) = env
//         .pubsub
//         .slot_subscribe()
//         .await
//         .expect("failed to subscribe to slots");

//     let initial_slot = env.latest_slot();

//     for i in 1..=3 {
//         // Trigger a new slot.
//         env.advance_slots(1);

//         // Await the notification and verify the slot number.
//         let notification =
//             tokio::time::timeout(Duration::from_secs(2), stream.next())
//                 .await
//                 .expect("timed out waiting for slot notification")
//                 .unwrap()
//                 .unwrap();

//         assert_eq!(notification.slot, initial_slot + i);
//         assert_eq!(notification.parent, initial_slot + i - 1);
//     }

//     unsub().await;
// }

// /// Verifies `logsSubscribe` receives logs from processed transactions.
// #[tokio::test]
// async fn test_logs_subscribe() {
//     let env = RpcTestEnv::new().await;

//     // Subscribe to all logs.
//     let (mut stream, unsub) = env
//         .pubsub
//         .logs_subscribe(RpcLogsFilter::All, Some(CommitmentConfig::processed()))
//         .await
//         .expect("failed to subscribe to logs");

//     // Execute a transaction that will produce logs.
//     let signature = env.execute_transaction().await;

//     // Await the log notification.
//     let notification =
//         tokio::time::timeout(Duration::from_secs(2), stream.next())
//             .await
//             .expect("timed out waiting for log notification")
//             .unwrap()
//             .unwrap();

//     // Verify the notification contents.
//     assert_eq!(notification.value.signature, signature.to_string());
//     assert!(notification.value.err.is_none());
//     assert!(
//         !notification.value.logs.is_empty(),
//         "log messages should be present"
//     );
//     assert!(notification
//         .value
//         .logs
//         .iter()
//         .any(|log| log.contains("Program log")));

//     unsub().await;
// }
