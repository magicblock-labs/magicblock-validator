use log::*;
use magicblock_chainlink::{
    config::ChainlinkConfig,
    config::LifecycleMode,
    remote_account_provider::config::RemoteAccountProviderConfig,
    testing::{init_logger, utils::random_pubkeys},
};

use test_chainlink::ixtest_context::IxtestContext;

async fn setup(
    subscribed_accounts_lru_capacity: usize,
    pubkeys_len: usize,
) -> (IxtestContext, Vec<solana_pubkey::Pubkey>) {
    let config = {
        let rap_config = RemoteAccountProviderConfig::try_new(
            subscribed_accounts_lru_capacity,
            LifecycleMode::Ephemeral,
        )
        .unwrap();
        ChainlinkConfig::new(rap_config)
    };
    let ctx = IxtestContext::init_with_config(config).await;

    let pubkeys = random_pubkeys(pubkeys_len);
    let payloads = pubkeys
        .iter()
        .enumerate()
        .map(|(sol, pubkey)| (*pubkey, sol as u64 + 1))
        .collect::<Vec<_>>();
    ctx.add_accounts(&payloads).await;

    (ctx, pubkeys)
}

#[tokio::test]
async fn ixtest_read_multiple_accounts_not_exceeding_capacity() {
    init_logger();

    let subscribed_accounts_lru_capacity = 5;
    let pubkeys_len = 5;
    let (ctx, pubkeys) =
        setup(subscribed_accounts_lru_capacity, pubkeys_len).await;

    ctx.chainlink.ensure_accounts(&pubkeys).await.unwrap();

    // Verify all accounts are present in the cache
    for pubkey in pubkeys {
        assert!(
            ctx.cloner.get_account(&pubkey).is_some(),
            "Account {pubkey} should be present in the cache"
        );
    }
}

#[tokio::test]
async fn ixtest_read_multiple_accounts_exceeding_capacity() {
    init_logger();

    let subscribed_accounts_lru_capacity = 5;
    let pubkeys_len = 8;
    let (ctx, pubkeys) =
        setup(subscribed_accounts_lru_capacity, pubkeys_len).await;

    let remove_len = pubkeys_len - subscribed_accounts_lru_capacity;

    debug!("{}", ctx.cloner.dump_account_keys(false));

    // NOTE: here we deal with a race condition that would never happen with large enough LRU
    // cache capacity
    // Basically if we add more accounts than the capacity in one go then the first ones
    // will be removed, but since they haven't been added yet that does nothing and
    // they get still added later right after. Therefore here we go in steps:
    ctx.chainlink.ensure_accounts(&pubkeys[0..4]).await.unwrap();
    ctx.chainlink.ensure_accounts(&pubkeys[4..8]).await.unwrap();

    debug!("{}", ctx.cloner.dump_account_keys(false));

    // Verify that the first added accounts are not present in the cache
    for pubkey in &pubkeys[..remove_len] {
        assert!(
            ctx.cloner.get_account(pubkey).is_none(),
            "Account {pubkey} should be not present in the cache"
        );
    }
    // Verify that the remaining accounts are present in the cache
    for pubkey in pubkeys[remove_len..].iter() {
        assert!(
            ctx.cloner.get_account(pubkey).is_some(),
            "Account {pubkey} should be present in the cache"
        );
    }
}
