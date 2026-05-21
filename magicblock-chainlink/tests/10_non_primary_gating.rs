use std::{path::Path, sync::Arc};

use magicblock_chainlink::{
    accounts_bank::mock::AccountsBankStub,
    chainlink::config::ChainlinkConfig,
    remote_account_provider::{
        chain_pubsub_client::ChainPubsubClient,
        chain_rpc_client::ChainRpcClientImpl,
        chain_updates_client::ChainUpdatesClient, Endpoint, Endpoints,
    },
    submux::SubMuxClient,
    testing::{cloner_stub::ClonerStub, init_logger},
    AccountFetchOrigin, Chainlink, ChainlinkPrimaryEnablement,
};
use magicblock_config::config::{ChainLinkConfig, LifecycleMode};
use magicblock_core::coordination_mode::switch_to_replica_mode;
use solana_account::Account;
use solana_program::clock::Slot;
use solana_pubkey::Pubkey;
use utils::{
    accounts::account_shared_with_owner_and_slot, test_context::TestContext,
};

mod utils;

type EndpointChainlink = Chainlink<
    ChainRpcClientImpl,
    SubMuxClient<ChainUpdatesClient>,
    AccountsBankStub,
    ClonerStub,
>;

async fn setup(slot: Slot) -> TestContext {
    init_logger();
    TestContext::init(slot).await
}

fn lifecycle_mode_for_startup_mode(mode: &str) -> LifecycleMode {
    match mode {
        "Replica" | "ReplicaOnly" | "StandBy" => LifecycleMode::Replica,
        _ => panic!("unexpected non-primary startup mode: {mode}"),
    }
}

async fn new_endpoint_chainlink(
    startup_mode: &str,
) -> (EndpointChainlink, Arc<AccountsBankStub>) {
    new_endpoint_chainlink_with_lifecycle_mode(lifecycle_mode_for_startup_mode(
        startup_mode,
    ))
    .await
}

async fn new_endpoint_chainlink_with_lifecycle_mode(
    lifecycle_mode: LifecycleMode,
) -> (EndpointChainlink, Arc<AccountsBankStub>) {
    init_logger();
    switch_to_replica_mode();
    let bank = Arc::<AccountsBankStub>::default();
    let cloner = Arc::new(ClonerStub::new(bank.clone()));
    let endpoints = Endpoints::from(
        [
            Endpoint::Rpc {
                url: "http://127.0.0.1:8899".to_string(),
                label: "local-rpc".to_string(),
            },
            Endpoint::WebSocket {
                url: "ws://127.0.0.1:8900".to_string(),
                label: "local-ws".to_string(),
            },
        ]
        .as_slice(),
    );
    let chainlink = Chainlink::<
        ChainRpcClientImpl,
        SubMuxClient<ChainUpdatesClient>,
        AccountsBankStub,
        ClonerStub,
    >::try_new_from_endpoints(
        &endpoints,
        Default::default(),
        &bank,
        &cloner,
        solana_keypair::Keypair::new(),
        ChainlinkConfig::default_with_lifecycle_mode(lifecycle_mode),
        &ChainLinkConfig::default(),
        Path::new("."),
    )
    .await
    .unwrap();

    (chainlink, bank)
}

async fn assert_no_runtime_or_remote_work(chainlink: &EndpointChainlink) {
    assert!(!chainlink.is_runtime_active().await);
    assert_eq!(chainlink.lifecycle_state_for_tests(), "disabled");
    assert_eq!(chainlink.active_fetch_count_for_tests().await, None);
}

async fn assert_public_calls_do_not_start_runtime(
    chainlink: &EndpointChainlink,
    bank: &AccountsBankStub,
) {
    let local_pubkey = Pubkey::new_unique();
    let local_account = account_shared_with_owner_and_slot(
        &Account {
            lamports: 2_000,
            ..Default::default()
        },
        Pubkey::new_unique(),
        1,
    );
    bank.insert(local_pubkey, local_account.clone());

    let ensure_pubkey = Pubkey::new_unique();
    let ensure_result = chainlink
        .ensure_accounts(
            &[ensure_pubkey],
            None,
            AccountFetchOrigin::GetMultipleAccounts,
            None,
        )
        .await
        .unwrap();
    assert!(ensure_result.not_found_on_chain.is_empty());
    assert!(ensure_result.missing_delegation_record.is_empty());
    assert_no_runtime_or_remote_work(chainlink).await;

    let fetched_accounts = chainlink
        .fetch_accounts(
            &[local_pubkey],
            AccountFetchOrigin::GetMultipleAccounts,
            None,
        )
        .await
        .unwrap();
    assert_eq!(fetched_accounts, vec![Some(local_account)]);
    assert_no_runtime_or_remote_work(chainlink).await;

    let undelegation_pubkey = Pubkey::new_unique();
    chainlink
        .undelegation_requested(undelegation_pubkey)
        .await
        .unwrap();
    assert_no_runtime_or_remote_work(chainlink).await;
}

#[tokio::test]
async fn new_endpoint_chainlink_starts_without_active_runtime() {
    let (chainlink, _) = new_endpoint_chainlink("Replica").await;

    assert_no_runtime_or_remote_work(&chainlink).await;
}

#[tokio::test]
async fn non_primary_startup_modes_keep_chainlink_runtime_disabled() {
    // Chainlink is constructed before replicated validators publish Primary
    // mode. Replica, ReplicaOnly, and StandBy startup all enter Replica
    // coordination and defer endpoint runtime creation: no runtime, no
    // subscriptions, and no streams/tasks are created until an explicit
    // primary enablement succeeds.
    for mode in ["Replica", "ReplicaOnly", "StandBy"] {
        let (chainlink, bank) = new_endpoint_chainlink(mode).await;
        assert_no_runtime_or_remote_work(&chainlink).await;
        assert_public_calls_do_not_start_runtime(&chainlink, &bank).await;
        assert_no_runtime_or_remote_work(&chainlink).await;
        drop(chainlink);
    }
}

#[tokio::test]
async fn repeated_enable_disable_leaves_no_runtime_or_mock_subscriptions() {
    let ctx = setup(21).await;

    assert_eq!(
        ctx.chainlink.enable_primary().await.unwrap(),
        ChainlinkPrimaryEnablement::Active
    );
    assert!(ctx.chainlink.is_runtime_active().await);

    ctx.chainlink.disable().await.unwrap();
    assert!(!ctx.chainlink.is_runtime_active().await);

    assert_eq!(
        ctx.chainlink.enable_primary().await.unwrap(),
        ChainlinkPrimaryEnablement::DisabledByConfig
    );
    ctx.chainlink.disable().await.unwrap();

    assert!(!ctx.chainlink.is_runtime_active().await);
    assert_eq!(ctx.chainlink.lifecycle_state_for_tests(), "disabled");
    assert_eq!(ctx.chainlink.active_fetch_count_for_tests().await, None);
    assert!(ctx.pubsub_client.subscriptions_union().is_empty());
    assert!(ctx.pubsub_client.subscribed_program_ids().is_empty());
}

#[tokio::test]
async fn offline_primary_enable_reports_disabled_by_config() {
    let (chainlink, _) =
        new_endpoint_chainlink_with_lifecycle_mode(LifecycleMode::Offline)
            .await;

    assert_eq!(
        chainlink.enable_primary().await.unwrap(),
        ChainlinkPrimaryEnablement::DisabledByConfig
    );
    assert!(!chainlink.is_runtime_active().await);
    assert_eq!(chainlink.lifecycle_state_for_tests(), "disabled");
}

#[tokio::test]
async fn public_calls_after_disable_do_not_recreate_runtime_or_subscriptions() {
    let slot = 22;
    let ctx = setup(slot).await;
    ctx.chainlink.disable().await.unwrap();

    let ensure_pubkey = Pubkey::new_unique();
    let ensure_owner = Pubkey::new_unique();
    let ensure_account = account_shared_with_owner_and_slot(
        &Account {
            lamports: 1_000,
            ..Default::default()
        },
        ensure_owner,
        slot,
    );
    ctx.rpc_client
        .add_account(ensure_pubkey, ensure_account.into());

    let ensure_result = ctx
        .chainlink
        .ensure_accounts(
            &[ensure_pubkey],
            None,
            AccountFetchOrigin::GetMultipleAccounts,
            None,
        )
        .await
        .unwrap();
    assert!(ensure_result.not_found_on_chain.is_empty());
    assert!(ensure_result.missing_delegation_record.is_empty());
    assert!(!ctx.chainlink.is_runtime_active().await);
    assert!(ctx.pubsub_client.subscriptions_union().is_empty());

    let local_pubkey = Pubkey::new_unique();
    let local_account = account_shared_with_owner_and_slot(
        &Account {
            lamports: 2_000,
            ..Default::default()
        },
        Pubkey::new_unique(),
        slot,
    );
    ctx.bank.insert(local_pubkey, local_account.clone());
    let fetched_accounts = ctx
        .chainlink
        .fetch_accounts(
            &[local_pubkey],
            AccountFetchOrigin::GetMultipleAccounts,
            None,
        )
        .await
        .unwrap();
    assert_eq!(fetched_accounts, vec![Some(local_account)]);
    assert!(!ctx.chainlink.is_runtime_active().await);
    assert!(ctx.pubsub_client.subscriptions_union().is_empty());

    ctx.chainlink
        .undelegation_requested(Pubkey::new_unique())
        .await
        .unwrap();
    assert!(!ctx.chainlink.is_runtime_active().await);
    assert!(ctx.pubsub_client.subscriptions_union().is_empty());
    assert!(ctx.pubsub_client.subscribed_program_ids().is_empty());
}
