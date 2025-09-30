use log::{debug, info};
use magicblock_chainlink::config::LifecycleMode;
use magicblock_chainlink::remote_account_provider::config::RemoteAccountProviderConfig;
use magicblock_chainlink::submux::SubMuxClient;
use magicblock_chainlink::{
    remote_account_provider::{
        chain_pubsub_client::ChainPubsubClientImpl,
        chain_rpc_client::ChainRpcClientImpl, Endpoint, RemoteAccountProvider,
        RemoteAccountUpdateSource,
    },
    testing::utils::{
        airdrop, await_next_slot, current_slot, dump_remote_account_lamports,
        dump_remote_account_update_source, get_remote_account_lamports,
        get_remote_account_update_sources, init_logger, random_pubkey,
        sleep_ms, PUBSUB_URL, RPC_URL,
    },
};
use solana_rpc_client_api::{
    client_error::ErrorKind, config::RpcAccountInfoConfig, request::RpcError,
};
use solana_sdk::commitment_config::CommitmentConfig;
use tokio::sync::mpsc;

async fn init_remote_account_provider() -> RemoteAccountProvider<
    ChainRpcClientImpl,
    SubMuxClient<ChainPubsubClientImpl>,
> {
    let (fwd_tx, _fwd_rx) = mpsc::channel(100);
    let endpoints = [Endpoint {
        rpc_url: RPC_URL.to_string(),
        pubsub_url: PUBSUB_URL.to_string(),
    }];
    RemoteAccountProvider::<
        ChainRpcClientImpl,
        SubMuxClient<ChainPubsubClientImpl>,
    >::try_new_from_urls(
        &endpoints,
        CommitmentConfig::confirmed(),
        fwd_tx,
        &RemoteAccountProviderConfig::default_with_lifecycle_mode(
            LifecycleMode::Ephemeral,
        ),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn ixtest_get_non_existing_account() {
    init_logger();

    let remote_account_provider = init_remote_account_provider().await;

    let pubkey = random_pubkey();
    let remote_account = remote_account_provider.try_get(pubkey).await.unwrap();
    assert!(!remote_account.is_found());
}

#[tokio::test]
async fn ixtest_existing_account_for_future_slot() {
    init_logger();

    let remote_account_provider = init_remote_account_provider().await;

    let pubkey = random_pubkey();
    let rpc_client = remote_account_provider.rpc_client();
    airdrop(rpc_client, &pubkey, 1_000_000).await;

    await_next_slot(rpc_client).await;

    let cs = current_slot(rpc_client).await;
    let res = rpc_client
        .get_account_with_config(
            &pubkey,
            RpcAccountInfoConfig {
                commitment: Some(CommitmentConfig::processed()),
                min_context_slot: Some(cs + 1),
                ..Default::default()
            },
        )
        .await;
    debug!("{cs} -> {res:#?}");
    assert!(res.is_err(), "Expected error for future slot account fetch");
    let err = res.unwrap_err();
    assert!(matches!(
        err.kind,
        ErrorKind::RpcError(RpcError::ForUser(_))
    ));
    assert!(err
        .to_string()
        .contains("Minimum context slot has not been reached"));
}

#[tokio::test]
async fn ixtest_get_existing_account_for_valid_slot() {
    init_logger();

    let remote_account_provider = init_remote_account_provider().await;

    let pubkey = random_pubkey();
    let rpc_client = remote_account_provider.rpc_client();
    airdrop(rpc_client, &pubkey, 1_000_000).await;

    {
        // Fetching immediately does not return the account yet
        let remote_account =
            remote_account_provider.try_get(pubkey).await.unwrap();
        assert!(!remote_account.is_found());
    }

    info!("Waiting for subscription update...");
    sleep_ms(1_500).await;

    {
        // After waiting for a bit the subscription update came in
        let cs = current_slot(rpc_client).await;
        let remote_account =
            remote_account_provider.try_get(pubkey).await.unwrap();
        assert!(remote_account.is_found());
        assert!(remote_account.slot() >= cs);
    }
}

#[tokio::test]
async fn ixtest_get_multiple_accounts_for_valid_slot() {
    init_logger();

    let remote_account_provider = init_remote_account_provider().await;

    let (pubkey1, pubkey2, pubkey3, pubkey4) = (
        random_pubkey(),
        random_pubkey(),
        random_pubkey(),
        random_pubkey(),
    );
    let rpc_client = remote_account_provider.rpc_client();

    airdrop(rpc_client, &pubkey1, 1_000_000).await;
    airdrop(rpc_client, &pubkey2, 2_000_000).await;
    airdrop(rpc_client, &pubkey3, 3_000_000).await;

    let all_pubkeys = vec![pubkey1, pubkey2, pubkey3, pubkey4];

    {
        // Fetching immediately does not return the accounts yet
        // They are updated via subscriptions instead
        let remote_accounts = remote_account_provider
            .try_get_multi(&all_pubkeys, None)
            .await
            .unwrap();

        let remote_lamports =
            get_remote_account_lamports(&all_pubkeys, &remote_accounts);
        dump_remote_account_lamports(&remote_lamports);

        assert_eq!(
            remote_accounts
                .iter()
                .map(|x| x.is_found())
                .collect::<Vec<_>>(),
            vec![false; 4]
        );
    }

    sleep_ms(500).await;
    await_next_slot(rpc_client).await;

    {
        // Fetching after a bit
        let remote_accounts = remote_account_provider
            .try_get_multi(&all_pubkeys, None)
            .await
            .unwrap();
        let remote_lamports =
            get_remote_account_lamports(&all_pubkeys, &remote_accounts);
        dump_remote_account_lamports(&remote_lamports);

        assert_eq!(
            remote_lamports,
            vec![
                (&pubkey1, 1_000_000),
                (&pubkey2, 2_000_000),
                (&pubkey3, 3_000_000),
                (&pubkey4, 0)
            ]
        );
    }

    // Create last account
    airdrop(rpc_client, &pubkey4, 4_000_000).await;
    // Update first account
    airdrop(rpc_client, &pubkey1, 111_111).await;

    sleep_ms(500).await;
    await_next_slot(rpc_client).await;

    {
        // Fetching after a bit
        let remote_accounts = remote_account_provider
            .try_get_multi(&all_pubkeys, None)
            .await
            .unwrap();
        let remote_lamports =
            get_remote_account_lamports(&all_pubkeys, &remote_accounts);
        dump_remote_account_lamports(&remote_lamports);

        assert_eq!(
            remote_lamports,
            vec![
                (&pubkey1, 1_111_111),
                (&pubkey2, 2_000_000),
                (&pubkey3, 3_000_000),
                (&pubkey4, 4_000_000)
            ]
        );

        let remote_sources =
            get_remote_account_update_sources(&all_pubkeys, &remote_accounts);
        dump_remote_account_update_source(&remote_sources);
        use RemoteAccountUpdateSource::Fetch;
        assert_eq!(
            remote_sources,
            vec![
                (&pubkey1, Some(Fetch)),
                (&pubkey2, Some(Fetch)),
                (&pubkey3, Some(Fetch)),
                (&pubkey4, Some(Fetch))
            ]
        );
    }
}
