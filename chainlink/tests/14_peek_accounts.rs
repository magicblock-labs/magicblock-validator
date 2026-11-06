#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use magicblock_chainlink::{
    assert_not_cloned, assert_not_subscribed, testing::context::TestContext,
};
use solana_account::{Account, AccountBuilder, AccountMode, ReadableAccount};
use solana_pubkey::Pubkey;

const CURRENT_SLOT: u64 = 11;

#[tokio::test]
async fn peek_accounts_reads_base_without_cloning_or_subscribing() {
    let ctx = TestContext::init(CURRENT_SLOT).await;
    let local = Pubkey::new_unique();
    ctx.bank
        .accounts()
        .store(&[(
            local,
            AccountBuilder::default()
                .lamports(1)
                .data(vec![9])
                .mode(AccountMode::Magic)
                .build(),
        )])
        .expect("store local account");

    let remote = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    ctx.rpc_client.add_account(
        remote,
        Account {
            lamports: 5,
            data: vec![1, 2, 3],
            owner,
            ..Default::default()
        },
    );
    let missing = Pubkey::new_unique();

    let clone_fetches = ctx.chainlink.fetch_count().unwrap();
    let rpc_fetches = ctx.rpc_client.multi_account_fetches();
    let peeked = ctx.chainlink.peek_accounts(&[local, remote, missing]).await.expect("peek");

    assert_eq!(peeked[0].as_ref().unwrap().data(), &[9]);
    assert_eq!(peeked[1].as_ref().unwrap().data(), &[1, 2, 3]);
    assert_eq!(*peeked[1].as_ref().unwrap().owner(), owner);
    assert!(peeked[2].is_none());
    assert_not_cloned!(ctx.bank, &[remote, missing]);
    assert_not_subscribed!(ctx.chainlink, &[&remote, &missing]);
    assert_eq!(ctx.chainlink.fetch_count().unwrap(), clone_fetches);
    assert_eq!(ctx.rpc_client.multi_account_fetches(), rpc_fetches + 1);

    let rpc_fetches = ctx.rpc_client.multi_account_fetches();
    let again = ctx.chainlink.peek_accounts(&[remote, missing]).await.expect("cached peek");
    assert_eq!(again[0].as_ref().unwrap().data(), &[1, 2, 3]);
    assert!(again[1].is_none());
    assert_eq!(
        ctx.rpc_client.multi_account_fetches(),
        rpc_fetches,
        "a second peek reuses the short-lived snapshot cache"
    );
    assert_not_cloned!(ctx.bank, &[remote, missing]);
    assert_not_subscribed!(ctx.chainlink, &[&remote, &missing]);
}
