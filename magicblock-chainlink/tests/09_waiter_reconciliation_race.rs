use magicblock_chainlink::{
    AccountFetchContext, assert_cloned_as_delegated, assert_not_subscribed,
    testing::{
        context::{TestChainlink, TestContext},
        deleg::add_delegation_record_for,
    },
};
use solana_account::Account;
use solana_pubkey::Pubkey;
use tracing::*;

const CURRENT_SLOT: u64 = 11;

async fn wait_for_registered_waiters(
    chainlink: &TestChainlink,
    account_pubkey: &Pubkey,
    expected_waiters: usize,
) {
    let fetch_cloner = chainlink
        .fetch_cloner()
        .expect("fetch cloner should be configured");
    let waiter_registration_start = tokio::time::Instant::now();
    let waiter_registration_timeout = tokio::time::Duration::from_secs(2);

    loop {
        if fetch_cloner
            .pending_request_waiter_count(account_pubkey)
            .is_some_and(|count| count >= expected_waiters)
        {
            break;
        }

        assert!(
            waiter_registration_start.elapsed() < waiter_registration_timeout,
            "pending_request_waiter_count for {account_pubkey} did not reach {expected_waiters} within {waiter_registration_timeout:?}"
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}

/// Integration test for waiter_reconciliation_check() race condition recovery.
///
/// Covers:
/// - concurrent delegated fetches
/// - deduped owner / pending-operation behavior
/// - the regression race where the owner clones the delegated terminal state
///   while waiters are already queued
#[tokio::test]
async fn test_owned_pending_operation_dedups_concurrent_delegated_fetches() {
    let TestContext {
        chainlink,
        rpc_client,
        cloner,
        bank: _,
        validator_pubkey,
        ..
    } = TestContext::init(CURRENT_SLOT).await;

    let account_pubkey = Pubkey::new_unique();
    let account_owner = Pubkey::new_unique();

    let chain_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };
    rpc_client.add_account(account_pubkey, chain_account.clone());

    let deleg_record_pubkey = add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
    );

    cloner.set_clone_delay(std::time::Duration::from_millis(200));
    cloner.block_clone_completion();

    let chainlink1 = chainlink.clone();
    let chainlink2 = chainlink.clone();
    let chainlink3 = chainlink.clone();

    let task1 = tokio::spawn(async move {
        info!("Task1: Starting fetch");
        chainlink1
            .ensure_accounts(
                &[account_pubkey],
                None,
                AccountFetchContext::rpc_get_multiple_accounts(),
            )
            .await
    });

    let task2 = tokio::spawn(async move {
        info!("Task2: Starting fetch");
        chainlink2
            .ensure_accounts(
                &[account_pubkey],
                None,
                AccountFetchContext::rpc_get_multiple_accounts(),
            )
            .await
    });

    let task3 = tokio::spawn(async move {
        info!("Task3: Starting fetch");
        chainlink3
            .ensure_accounts(
                &[account_pubkey],
                None,
                AccountFetchContext::rpc_get_multiple_accounts(),
            )
            .await
    });

    wait_for_registered_waiters(chainlink.as_ref(), &account_pubkey, 2).await;
    cloner.allow_clone_completion();

    let (result1, result2, result3) =
        tokio::try_join!(task1, task2, task3).expect("Tasks should complete");

    info!("Result1: {:?}", result1);
    info!("Result2: {:?}", result2);
    info!("Result3: {:?}", result3);

    assert!(result1.is_ok());
    assert!(result2.is_ok());
    assert!(result3.is_ok());
    assert_eq!(cloner.clone_request_count(), 1);

    assert_cloned_as_delegated!(
        cloner,
        &[account_pubkey],
        CURRENT_SLOT,
        account_owner
    );
    assert_not_subscribed!(chainlink, &[&account_pubkey, &deleg_record_pubkey]);
}

/// Integration test verifying concurrent requests with a real owner/waiter race.
#[tokio::test]
async fn test_multiple_concurrent_requests_with_valid_delegated_state() {
    let TestContext {
        chainlink,
        rpc_client,
        cloner,
        bank: _,
        validator_pubkey,
        ..
    } = TestContext::init(CURRENT_SLOT).await;

    let account_pubkey = Pubkey::new_unique();
    let account_owner = Pubkey::new_unique();

    let chain_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };
    rpc_client.add_account(account_pubkey, chain_account.clone());

    let deleg_record_pubkey = add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
    );

    cloner.set_clone_delay(std::time::Duration::from_millis(200));
    cloner.block_clone_completion();

    let chainlink1 = chainlink.clone();
    let chainlink2 = chainlink.clone();
    let chainlink3 = chainlink.clone();

    let task1 = tokio::spawn(async move {
        chainlink1
            .ensure_accounts(
                &[account_pubkey],
                None,
                AccountFetchContext::rpc_get_multiple_accounts(),
            )
            .await
    });

    let task2 = tokio::spawn(async move {
        chainlink2
            .ensure_accounts(
                &[account_pubkey],
                None,
                AccountFetchContext::rpc_get_multiple_accounts(),
            )
            .await
    });

    let task3 = tokio::spawn(async move {
        chainlink3
            .ensure_accounts(
                &[account_pubkey],
                None,
                AccountFetchContext::rpc_get_multiple_accounts(),
            )
            .await
    });

    wait_for_registered_waiters(chainlink.as_ref(), &account_pubkey, 2).await;
    cloner.allow_clone_completion();

    let (result1, result2, result3) =
        tokio::try_join!(task1, task2, task3).expect("Tasks should complete");
    assert!(result1.is_ok());
    assert!(result2.is_ok());
    assert!(result3.is_ok());
    assert_eq!(cloner.clone_request_count(), 1);

    assert_cloned_as_delegated!(
        cloner,
        &[account_pubkey],
        CURRENT_SLOT,
        account_owner
    );
    assert_not_subscribed!(chainlink, &[&account_pubkey, &deleg_record_pubkey]);
}
