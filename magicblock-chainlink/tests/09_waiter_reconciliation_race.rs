use magicblock_chainlink::{
    assert_cloned_as_delegated, assert_not_subscribed,
    testing::{
        accounts::delegated_account_shared_with_owner_and_slot,
        deleg::add_delegation_record_for, init_logger,
    },
    AccountFetchOrigin,
};
use solana_account::Account;
use solana_program::clock::Slot;
use solana_pubkey::Pubkey;
use tracing::*;
use utils::test_context::TestContext;

mod utils;

const CURRENT_SLOT: u64 = 11;

async fn setup(slot: Slot) -> TestContext {
    init_logger();
    TestContext::init(slot).await
}

/// Integration test for waiter_reconciliation_check() race condition recovery.
///
/// Tests the stuck pubkey scenario where:
/// 1. Account is pre-populated in the bank as delegated (simulating subscription arriving early)
/// 2. Multiple concurrent tasks try to fetch the same account
/// 3. Waiter tasks should detect the valid delegated terminal state and accept it
///    without triggering unnecessary fresh fetch
///
/// This test covers the full system-level behavior of the waiter_reconciliation_check()
/// function in detecting valid terminal state and completing successfully.
#[tokio::test]
async fn test_waiter_reconciliation_detects_valid_delegated_state() {
    let TestContext {
        chainlink,
        rpc_client,
        cloner,
        bank,
        validator_pubkey,
        ..
    } = setup(CURRENT_SLOT).await;

    let account_pubkey = Pubkey::new_unique();
    let account_owner = Pubkey::new_unique();

    // Setup account on chain (owned by delegation program, delegated to us)
    let chain_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };
    rpc_client.add_account(account_pubkey, chain_account.clone());

    // Add delegation record delegated to us
    let deleg_record_pubkey =
        add_delegation_record_for(&rpc_client, account_pubkey, validator_pubkey, account_owner);

    // Pre-populate bank with delegated account (simulating subscription update arriving early)
    // The account's owner field should be set to the delegation record owner, not dlp_api::id()
    let delegated_account =
        delegated_account_shared_with_owner_and_slot(&chain_account, account_owner, CURRENT_SLOT);
    bank.insert(account_pubkey, delegated_account);

    // Spawn concurrent fetch tasks to simulate owner/waiter pattern
    let chainlink1 = chainlink.clone();
    let chainlink2 = chainlink.clone();

    // Task 1: Owner - performs the initial fetch+clone
    let task1 = tokio::spawn(async move {
        info!("Task1: Starting fetch");
        chainlink1
            .ensure_accounts(
                &[account_pubkey],
                None,
                AccountFetchOrigin::GetMultipleAccounts,
                None,
            )
            .await
    });

    // Task 2: Waiter - also tries to fetch the same account
    // This waiter should block on dedup if Task1 is in progress
    let task2 = tokio::spawn(async move {
        info!("Task2: Starting fetch");
        // Small delay to ensure task1 starts first
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        chainlink2
            .ensure_accounts(
                &[account_pubkey],
                None,
                AccountFetchOrigin::GetMultipleAccounts,
                None,
            )
            .await
    });

    // Wait for both tasks to complete
    let (result1, result2) = tokio::try_join!(task1, task2).expect("Tasks should complete");

    info!("Result1: {:?}", result1);
    info!("Result2: {:?}", result2);

    // Both should succeed
    assert!(result1.is_ok());
    assert!(result2.is_ok());

    // Account should remain delegated in bank
    assert_cloned_as_delegated!(cloner, &[account_pubkey], CURRENT_SLOT, account_owner);

    // Should not be subscribed to either account or delegation record
    assert_not_subscribed!(chainlink, &[&account_pubkey, &deleg_record_pubkey]);
}



/// Integration test verifying concurrent requests with pre-populated valid state.
///
/// Tests the scenario where:
/// 1. Account is pre-populated in bank as delegated at current slot
/// 2. Multiple concurrent fetch requests happen
/// 3. All requests should succeed with account in valid terminal state
#[tokio::test]
async fn test_multiple_concurrent_requests_with_valid_delegated_state() {
    let TestContext {
        chainlink,
        rpc_client,
        cloner,
        bank,
        validator_pubkey,
        ..
    } = setup(CURRENT_SLOT).await;

    let account_pubkey = Pubkey::new_unique();
    let account_owner = Pubkey::new_unique();

    // Setup account on chain
    let chain_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };
    rpc_client.add_account(account_pubkey, chain_account.clone());

    // Add delegation record
    let deleg_record_pubkey =
        add_delegation_record_for(&rpc_client, account_pubkey, validator_pubkey, account_owner);

    // Pre-populate bank with delegated account at current slot
    let delegated_account =
        delegated_account_shared_with_owner_and_slot(&chain_account, account_owner, CURRENT_SLOT);
    bank.insert(account_pubkey, delegated_account);

    // Spawn 3 concurrent fetch requests to verify they all succeed
    let chainlink1 = chainlink.clone();
    let chainlink2 = chainlink.clone();
    let chainlink3 = chainlink.clone();

    let task1 = tokio::spawn(async move {
        chainlink1
            .ensure_accounts(
                &[account_pubkey],
                None,
                AccountFetchOrigin::GetMultipleAccounts,
                None,
            )
            .await
    });

    let task2 = tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        chainlink2
            .ensure_accounts(
                &[account_pubkey],
                None,
                AccountFetchOrigin::GetMultipleAccounts,
                None,
            )
            .await
    });

    let task3 = tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
        chainlink3
            .ensure_accounts(
                &[account_pubkey],
                None,
                AccountFetchOrigin::GetMultipleAccounts,
                None,
            )
            .await
    });

    // All tasks should complete successfully
    let (result1, result2, result3) = tokio::try_join!(task1, task2, task3).expect("Tasks should complete");
    assert!(result1.is_ok());
    assert!(result2.is_ok());
    assert!(result3.is_ok());

    // Account should remain delegated at current slot
    assert_cloned_as_delegated!(cloner, &[account_pubkey], CURRENT_SLOT, account_owner);

    // Should not be subscribed
    assert_not_subscribed!(chainlink, &[&account_pubkey, &deleg_record_pubkey]);
}
