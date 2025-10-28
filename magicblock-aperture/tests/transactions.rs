use std::time::Duration;

use magicblock_core::{link::blocks::BlockHash, traits::AccountsBank};
use setup::RpcTestEnv;
use solana_account::ReadableAccount;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::GetConfirmedSignaturesForAddress2Config;
use solana_rpc_client_api::config::{
    RpcSendTransactionConfig, RpcSimulateTransactionConfig,
};
use solana_signature::Signature;
use solana_transaction_status::UiTransactionEncoding;
use test_kit::guinea;

mod setup;

// --- sendTransaction Tests ---

/// Verifies that a simple, valid transaction is successfully processed.
#[tokio::test]
async fn test_send_transaction_success() {
    let env = RpcTestEnv::new().await;
    let sender = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let transfer_tx =
        env.build_transfer_txn_with_params(sender, recipient, false);
    let config = RpcSendTransactionConfig {
        encoding: Some(UiTransactionEncoding::Base58),
        ..Default::default()
    };

    let signature = env
        .rpc
        .send_transaction_with_config(&transfer_tx, config)
        .await
        .expect("send_transaction failed for a valid transaction");

    let meta = env
        .execution
        .get_transaction(signature)
        .expect("failed to retrieve executed transaction meta from ledger");
    assert!(
        meta.status.is_ok(),
        "transaction should have executed successfully"
    );

    let sender_account = env.execution.accountsdb.get_account(&sender).unwrap();
    let recipient_account =
        env.execution.accountsdb.get_account(&recipient).unwrap();

    assert_eq!(
        sender_account.lamports(),
        RpcTestEnv::INIT_ACCOUNT_BALANCE - RpcTestEnv::TRANSFER_AMOUNT,
        "sender account balance was not properly debited"
    );
    assert_eq!(
        recipient_account.lamports(),
        RpcTestEnv::INIT_ACCOUNT_BALANCE + RpcTestEnv::TRANSFER_AMOUNT,
        "recipient account balance was not properly credited"
    );
}

/// Verifies the higher-level `send_and_confirm_transaction` method works correctly,
/// particularly with preflight checks skipped.
#[tokio::test]
async fn test_send_and_confirm_transaction_success() {
    let env = RpcTestEnv::new().await;
    let transfer_tx = env.build_transfer_txn();
    let config = RpcSendTransactionConfig {
        skip_preflight: true, // Test with preflight checks disabled
        encoding: Some(UiTransactionEncoding::Base64),
        ..Default::default()
    };

    let signature = env
        .rpc
        .send_and_confirm_transaction_with_spinner_and_config(
            &transfer_tx,
            Default::default(),
            config,
        )
        .await
        .expect("send_and_confirm_transaction failed");

    let meta = env
        .execution
        .get_transaction(signature)
        .expect("failed to retrieve executed transaction meta from ledger");
    assert!(
        meta.status.is_ok(),
        "transaction should have executed successfully"
    );
}

/// Ensures the validator rejects a transaction sent twice (replay attack).
#[tokio::test]
async fn test_send_transaction_replay_attack() {
    let env = RpcTestEnv::new().await;
    let transfer_tx = env.build_transfer_txn();

    env.rpc
        .send_transaction(&transfer_tx)
        .await
        .expect("first transaction send should have succeeded");

    let replay_result = env.rpc.send_transaction(&transfer_tx).await;

    assert!(
        replay_result.is_err(),
        "second identical transaction should fail"
    );
}

/// Verifies a transaction with an invalid blockhash is rejected.
#[tokio::test]
async fn test_send_transaction_with_invalid_blockhash() {
    let env = RpcTestEnv::new().await;
    let mut transfer_tx = env.build_transfer_txn();
    transfer_tx.message.recent_blockhash = BlockHash::new_unique(); // Use a bogus blockhash
    let signature = transfer_tx.signatures[0];

    let result = env.rpc.send_transaction(&transfer_tx).await;

    assert!(
        result.is_err(),
        "transaction with an invalid blockhash should fail"
    );
    assert!(
        env.execution.get_transaction(signature).is_none(),
        "failed transaction should not be persisted to the ledger"
    );
}

/// Verifies a transaction with an invalid signature is rejected.
#[tokio::test]
async fn test_send_transaction_with_invalid_signature() {
    let env = RpcTestEnv::new().await;
    let mut transfer_tx = env.build_transfer_txn();
    let signature = Signature::new_unique();
    transfer_tx.signatures[0] = signature; // Use a bogus signature
    let config = RpcSendTransactionConfig {
        skip_preflight: true, // Skip preflight to test deeper validation
        ..Default::default()
    };

    let result = env
        .rpc
        .send_transaction_with_config(&transfer_tx, config)
        .await;

    assert!(
        result.is_err(),
        "transaction with an invalid signature should fail"
    );
    assert!(
        env.execution.get_transaction(signature).is_none(),
        "failed transaction should not be persisted to the ledger"
    );
}

// --- simulateTransaction Tests ---

/// Verifies a valid transaction can be successfully simulated without changing state.
#[tokio::test]
async fn test_simulate_transaction_success() {
    let env = RpcTestEnv::new().await;
    let sender = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let transfer_tx =
        env.build_transfer_txn_with_params(sender, recipient, false);
    let signature = transfer_tx.signatures[0];

    let result = env
        .rpc
        .simulate_transaction(&transfer_tx)
        .await
        .expect("simulate_transaction request failed")
        .value;

    assert!(
        env.execution.get_transaction(signature).is_none(),
        "simulated transaction should not be persisted"
    );
    assert!(
        result.err.is_none(),
        "valid transaction simulation should not produce an error"
    );
    assert!(
        result.units_consumed.unwrap_or_default() > 0,
        "simulation should consume compute units"
    );

    // Critically, verify account balances were not affected.
    let sender_account = env.execution.accountsdb.get_account(&sender).unwrap();
    let recipient_account =
        env.execution.accountsdb.get_account(&recipient).unwrap();
    assert_eq!(
        sender_account.lamports(),
        RpcTestEnv::INIT_ACCOUNT_BALANCE,
        "sender balance should not change after simulation"
    );
    assert_eq!(
        recipient_account.lamports(),
        RpcTestEnv::INIT_ACCOUNT_BALANCE,
        "recipient balance should not change after simulation"
    );
}

/// Tests simulation with config options like replacing blockhash and skipping signature verification.
#[tokio::test]
async fn test_simulate_transaction_with_config_options() {
    let env = RpcTestEnv::new().await;

    // Test `replace_recent_blockhash: true`
    {
        let mut transfer_tx = env.build_transfer_txn();
        let bogus_blockhash = BlockHash::new_unique();
        transfer_tx.message.recent_blockhash = bogus_blockhash;

        let config = RpcSimulateTransactionConfig {
            sig_verify: true,
            replace_recent_blockhash: true,
            ..Default::default()
        };
        let result = env
            .rpc
            .simulate_transaction_with_config(&transfer_tx, config)
            .await
            .expect("simulation with blockhash replacement failed")
            .value;

        assert!(
            result.err.is_none(),
            "simulation with replaced blockhash should succeed"
        );
        assert!(
            result
                .replacement_blockhash
                .map(|bh| bh.blockhash != bogus_blockhash.to_string())
                .unwrap_or(false),
            "blockhash must have been replaced with a recent one"
        );
    }

    // Test `sig_verify: false`
    {
        let mut transfer_tx = env.build_transfer_txn();
        transfer_tx.signatures[0] = Signature::new_unique(); // Invalid signature

        let config = RpcSimulateTransactionConfig {
            sig_verify: false, // Skip signature verification
            ..Default::default()
        };
        let result = env
            .rpc
            .simulate_transaction_with_config(&transfer_tx, config)
            .await
            .expect("simulation with sig_verify=false failed")
            .value;

        assert!(
            result.err.is_none(),
            "simulation without signature verification should succeed"
        );
    }
}

/// Verifies that simulating an invalid transaction correctly returns an error.
#[tokio::test]
async fn test_simulate_transaction_failure() {
    let env = RpcTestEnv::new().await;

    // Test with an instruction that is guaranteed to fail (e.g., insufficient funds).
    let failing_tx = env.build_failing_transfer_txn();
    let result = env
        .rpc
        .simulate_transaction(&failing_tx)
        .await
        .expect("simulate_transaction request itself should not fail")
        .value;

    assert!(
        result.err.is_some(),
        "invalid transaction simulation should have returned an error"
    );
}

// --- requestAirdrop & getFeeForMessage Tests ---

/// Verifies that airdrops correctly fund an account.
#[tokio::test]
async fn test_request_airdrop() {
    let env = RpcTestEnv::new().await;
    let recipient = Pubkey::new_unique();
    env.execution.fund_account(recipient, 1); // Start with 1 lamport
    let airdrop_amount = RpcTestEnv::INIT_ACCOUNT_BALANCE / 10;

    let signature = env
        .rpc
        .request_airdrop(&recipient, airdrop_amount)
        .await
        .expect("request_airdrop failed");

    let meta = env
        .execution
        .get_transaction(signature)
        .expect("airdrop transaction should have been persisted");
    assert!(meta.status.is_ok(), "airdrop transaction should succeed");

    let account = env.execution.accountsdb.get_account(&recipient).unwrap();
    assert_eq!(
        account.lamports(),
        airdrop_amount + 1,
        "airdrop was not credited correctly"
    );
}

/// Verifies that `get_fee_for_message` returns the correct fee based on the number of signatures.
#[tokio::test]
async fn test_get_fee_for_message() {
    let env = RpcTestEnv::new().await;
    let transfer_tx = env.build_transfer_txn();

    let fee = env
        .rpc
        .get_fee_for_message(&transfer_tx.message)
        .await
        .expect("get_fee_for_message failed");

    assert_eq!(fee, RpcTestEnv::BASE_FEE);
}

// --- Signature and Transaction History Tests ---

/// Verifies `get_signature_statuses` for successful, failed, and non-existent transactions.
#[tokio::test]
async fn test_get_signature_statuses() {
    let env = RpcTestEnv::new().await;
    let sig_success = env.execute_transaction().await;
    let failing_tx = env.build_failing_transfer_txn();
    let sig_fail = failing_tx.signatures[0];
    env.execution
        .transaction_scheduler
        .schedule(failing_tx)
        .await
        .unwrap();
    let sig_nonexistent = Signature::new_unique();
    tokio::time::sleep(Duration::from_millis(10)).await; // Allow propagation

    let statuses = env
        .rpc
        .get_signature_statuses(&[sig_success, sig_fail, sig_nonexistent])
        .await
        .expect("get_signature_statuses request failed")
        .value;

    assert_eq!(
        statuses.len(),
        3,
        "should return status for all 3 signatures"
    );

    let status_success = statuses[0].clone().unwrap();
    assert_eq!(status_success.status, Ok(()));

    let status_fail = statuses[1].clone().unwrap();
    assert!(status_fail.status.is_err());

    assert!(
        statuses[2].is_none(),
        "status for non-existent signature should be None"
    );
}

/// Verifies `get_signatures_for_address` finds all relevant transactions.
#[tokio::test]
async fn test_get_signatures_for_address() {
    let env = RpcTestEnv::new().await;
    let signature1 = env.execute_transaction().await;
    let signature2 = env.execute_transaction().await;

    let signatures = env
        .rpc
        .get_signatures_for_address(&guinea::ID)
        .await
        .expect("get_signatures_for_address failed");

    assert!(signatures.len() >= 2, "should find at least two signatures");
    let sig_strings: Vec<_> =
        signatures.iter().map(|s| s.signature.clone()).collect();
    assert!(sig_strings.contains(&signature1.to_string()));
    assert!(sig_strings.contains(&signature2.to_string()));
}

/// Verifies pagination (`before` and `until`) for `get_signatures_for_address`.
#[tokio::test]
async fn test_get_signatures_for_address_pagination() {
    let env = RpcTestEnv::new().await;
    let mut signatures = Vec::new();
    for _ in 0..5 {
        env.advance_slots(1);
        signatures.push(env.execute_transaction().await);
    }

    // Test `before`: Get 2 signatures that occurred before the 4th transaction.
    let config_before = GetConfirmedSignaturesForAddress2Config {
        before: Some(signatures[3]), // 4th signature
        limit: Some(2),
        ..Default::default()
    };
    let result_before = env
        .rpc
        .get_signatures_for_address_with_config(&guinea::ID, config_before)
        .await
        .unwrap();

    assert_eq!(result_before.len(), 2);
    assert_eq!(result_before[0].signature, signatures[2].to_string()); // 3rd tx
    assert_eq!(result_before[1].signature, signatures[1].to_string()); // 2nd tx

    // Test `until`: Get all signatures that occurred after the 2nd transaction.
    let config_until = GetConfirmedSignaturesForAddress2Config {
        until: Some(signatures[1]), // 2nd signature
        ..Default::default()
    };
    let result_until = env
        .rpc
        .get_signatures_for_address_with_config(&guinea::ID, config_until)
        .await
        .unwrap();

    assert_eq!(result_until.len(), 3);
    assert_eq!(result_until[0].signature, signatures[4].to_string()); // 5th tx
    assert_eq!(result_until[1].signature, signatures[3].to_string()); // 4th tx
    assert_eq!(result_until[2].signature, signatures[2].to_string()); // 3rd tx
}

/// Verifies `get_transaction` for both successful and failed transactions.
#[tokio::test]
async fn test_get_transaction() {
    // Test successful transaction
    let env = RpcTestEnv::new().await;
    let success_sig = env.execute_transaction().await;
    let transaction = env
        .rpc
        .get_transaction(&success_sig, UiTransactionEncoding::Base64)
        .await
        .expect("getTransaction request failed");
    assert_eq!(transaction.slot, env.latest_slot());
    assert_eq!(transaction.transaction.meta.unwrap().err, None);

    // Test failed transaction
    let failing_tx = env.build_failing_transfer_txn();
    let fail_sig = failing_tx.signatures[0];
    env.execution
        .transaction_scheduler
        .schedule(failing_tx)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let transaction = env
        .rpc
        .get_transaction(&fail_sig, UiTransactionEncoding::Base64)
        .await
        .expect("getTransaction request failed");
    assert!(transaction.transaction.meta.unwrap().err.is_some());
}
