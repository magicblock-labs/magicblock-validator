use magicblock_chainlink::{
    AccountFetchContext, assert_cloned_as_delegated, assert_not_subscribed,
    testing::{context::TestContext, deleg::add_delegation_record_for},
};
use solana_account::Account;
use solana_pubkey::Pubkey;

const CURRENT_SLOT: u64 = 11;

#[tokio::test]
async fn concurrent_delegated_ensures_share_engine_load() {
    let TestContext {
        chainlink,
        rpc_client,
        bank,
        validator_pubkey,
        test_engine: _test_engine,
        ..
    } = TestContext::init(CURRENT_SLOT).await;

    let account_pubkey = Pubkey::new_unique();
    let account_owner = Pubkey::new_unique();
    rpc_client.add_account(
        account_pubkey,
        Account {
            lamports: 1_000_000,
            data: vec![1, 2, 3, 4],
            owner: dlp_api::id(),
            ..Default::default()
        },
    );
    let deleg_record_pubkey = add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
    );

    let ensure = || {
        let chainlink = chainlink.clone();
        tokio::spawn(async move {
            chainlink
                .ensure_accounts(
                    &[account_pubkey],
                    None,
                    AccountFetchContext::rpc_get_multiple_accounts(),
                )
                .await
        })
    };
    let (first, second, third) =
        tokio::try_join!(ensure(), ensure(), ensure()).expect("join ensures");
    first.expect("first ensure succeeds");
    second.expect("second ensure succeeds");
    third.expect("third ensure succeeds");

    assert_eq!(
        chainlink.fetch_count(),
        Some(3),
        "delegated resolution should perform only one owner fetch sequence"
    );
    assert_cloned_as_delegated!(
        bank,
        &[account_pubkey],
        CURRENT_SLOT,
        account_owner
    );
    assert_not_subscribed!(chainlink, &[&account_pubkey, &deleg_record_pubkey]);
}
