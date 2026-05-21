use magicblock_accounts_db::traits::AccountsBank;
use magicblock_chainlink::{
    testing::{deleg::add_delegation_record_for, init_logger},
    AccountFetchOrigin, PrimaryEnableOutcome, PrimaryRuntimeReadiness,
};
use solana_account::{Account, ReadableAccount};
use solana_pubkey::Pubkey;
use tokio::time::{sleep, Duration};
use utils::{
    accounts::account_shared_with_owner_and_slot, test_context::TestContext,
};

mod utils;

async fn setup(slot: u64) -> TestContext {
    init_logger();
    TestContext::init(slot).await
}

fn account_with_lamports(lamports: u64, owner: Pubkey, slot: u64) -> Account {
    account_shared_with_owner_and_slot(
        &Account {
            lamports,
            ..Default::default()
        },
        owner,
        slot,
    )
    .into()
}

#[tokio::test]
async fn chainlink_runtime_is_active_after_primary_enable() {
    let ctx = setup(1).await;

    assert!(ctx.chainlink.is_runtime_active().await);
    assert_eq!(
        ctx.chainlink.primary_runtime_readiness().await,
        PrimaryRuntimeReadiness::Ready
    );
    assert_eq!(
        ctx.chainlink.enable_primary().await.unwrap(),
        PrimaryEnableOutcome::RuntimeActive
    );
    assert!(ctx.chainlink.is_runtime_active().await);
}

#[tokio::test]
async fn disable_after_enable_stops_runtime_and_is_idempotent() {
    let ctx = setup(2).await;
    assert!(ctx.chainlink.is_runtime_active().await);

    ctx.chainlink.disable().await.unwrap();
    assert!(!ctx.chainlink.is_runtime_active().await);
    assert_eq!(
        ctx.chainlink.primary_runtime_readiness().await,
        PrimaryRuntimeReadiness::DisabledByConfig
    );

    ctx.chainlink.disable().await.unwrap();
    assert!(!ctx.chainlink.is_runtime_active().await);
}

#[tokio::test]
async fn active_runtime_fetches_before_shutdown_and_not_after() {
    let slot = 3;
    let ctx = setup(slot).await;
    let pubkey = Pubkey::new_unique();
    let owner = Pubkey::new_unique();

    ctx.rpc_client
        .add_account(pubkey, account_with_lamports(1_000, owner, slot));
    let before_fetch_count = ctx.chainlink.active_fetch_count_for_tests().await;
    ctx.chainlink
        .ensure_accounts(
            &[pubkey],
            None,
            AccountFetchOrigin::GetMultipleAccounts,
            None,
        )
        .await
        .unwrap();
    assert!(
        ctx.chainlink.active_fetch_count_for_tests().await > before_fetch_count
    );
    assert!(ctx.chainlink.is_watching(&pubkey));

    ctx.chainlink.disable().await.unwrap();
    assert!(!ctx.chainlink.is_runtime_active().await);

    let after_disable_fetch_count =
        ctx.chainlink.active_fetch_count_for_tests().await;
    ctx.chainlink
        .ensure_accounts(
            &[Pubkey::new_unique()],
            None,
            AccountFetchOrigin::GetMultipleAccounts,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        ctx.chainlink.active_fetch_count_for_tests().await,
        after_disable_fetch_count
    );
}

#[tokio::test]
async fn provider_account_updates_do_not_clone_after_disable() {
    let slot = 4;
    let ctx = setup(slot).await;
    let pubkey = Pubkey::new_unique();
    let owner = Pubkey::new_unique();

    ctx.rpc_client
        .add_account(pubkey, account_with_lamports(1_000, owner, slot));
    ctx.ensure_account(&pubkey).await.unwrap();
    assert!(ctx.chainlink.is_watching(&pubkey));

    ctx.rpc_client.set_slot(slot + 1);
    assert!(
        ctx.send_and_receive_account_update(
            pubkey,
            Account {
                lamports: 2_000,
                ..Default::default()
            },
            Some(400),
        )
        .await
    );
    assert_eq!(ctx.cloner.get_account(&pubkey).unwrap().lamports(), 2_000);
    let clone_requests_before_disable = ctx.cloner.clone_request_count();

    ctx.chainlink.disable().await.unwrap();
    ctx.rpc_client.set_slot(slot + 2);
    ctx.send_account_update(
        pubkey,
        &Account {
            lamports: 3_000,
            ..Default::default()
        },
    )
    .await;
    sleep(Duration::from_millis(50)).await;

    assert_eq!(
        ctx.cloner.clone_request_count(),
        clone_requests_before_disable
    );
    assert_eq!(ctx.cloner.get_account(&pubkey).unwrap().lamports(), 2_000);
}

#[tokio::test]
async fn removed_account_notifications_do_not_evict_after_disable() {
    let slot = 5;
    let ctx = TestContext::init_with_lru_capacity(slot, Some(1)).await;
    let first_pubkey = Pubkey::new_unique();
    let second_pubkey = Pubkey::new_unique();
    let third_pubkey = Pubkey::new_unique();
    let owner = Pubkey::new_unique();

    ctx.rpc_client
        .add_account(first_pubkey, account_with_lamports(1_000, owner, slot));
    ctx.ensure_account(&first_pubkey).await.unwrap();
    assert!(ctx.bank.get_account(&first_pubkey).is_some());

    ctx.rpc_client.add_account(
        second_pubkey,
        account_with_lamports(2_000, owner, slot + 1),
    );
    ctx.ensure_account(&second_pubkey).await.unwrap();
    sleep(Duration::from_millis(50)).await;
    assert!(
        ctx.bank.get_account(&first_pubkey).is_none(),
        "first LRU eviction should remove the account before shutdown"
    );

    ctx.rpc_client
        .add_account(third_pubkey, account_with_lamports(3_000, owner, slot));
    ctx.ensure_account(&third_pubkey).await.unwrap();
    assert!(ctx.bank.get_account(&third_pubkey).is_some());

    ctx.chainlink.disable().await.unwrap();
    sleep(Duration::from_millis(50)).await;
    assert!(
        ctx.bank.get_account(&third_pubkey).is_some(),
        "shutdown should stop removed-account eviction handling"
    );
}

#[tokio::test]
async fn delegated_account_can_be_fetched_through_fresh_runtime_after_shutdown()
{
    let slot = 6;
    let ctx = setup(slot).await;
    let pubkey = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    ctx.rpc_client
        .add_account(pubkey, account_with_lamports(1_000, dlp_api::id(), slot));
    add_delegation_record_for(
        &ctx.rpc_client,
        pubkey,
        ctx.validator_pubkey,
        owner,
    );

    ctx.chainlink.disable().await.unwrap();
    assert!(!ctx.chainlink.is_runtime_active().await);

    let fresh_ctx = setup(slot).await;
    fresh_ctx
        .rpc_client
        .add_account(pubkey, account_with_lamports(1_000, dlp_api::id(), slot));
    add_delegation_record_for(
        &fresh_ctx.rpc_client,
        pubkey,
        fresh_ctx.validator_pubkey,
        owner,
    );
    fresh_ctx.ensure_account(&pubkey).await.unwrap();
    assert!(fresh_ctx.chainlink.is_runtime_active().await);
    assert!(fresh_ctx.cloner.get_account(&pubkey).is_some());
}
