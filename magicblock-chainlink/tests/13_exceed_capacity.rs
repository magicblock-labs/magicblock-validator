use magicblock_chainlink::{
    AccountFetchContext, assert_cloned_as_delegated,
    testing::{
        context::TestContext, deleg::add_delegation_record_for, init_logger,
        utils::random_pubkeys,
    },
};
use solana_account::Account;
use solana_pubkey::Pubkey;

#[tokio::test]
async fn delegated_account_survives_readonly_cache_pressure() {
    init_logger();
    let ctx = TestContext::init_with_lru_capacity(100, 8).await;
    let delegated = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    ctx.rpc_client.add_account(
        delegated,
        Account {
            lamports: 1_000_000,
            owner: dlp_api::id(),
            ..Default::default()
        },
    );
    add_delegation_record_for(
        &ctx.rpc_client,
        delegated,
        ctx.validator_pubkey,
        owner,
    );
    ctx.chainlink
        .ensure_accounts(
            &[delegated],
            AccountFetchContext::rpc_get_multiple_accounts(),
        )
        .await
        .unwrap();

    let readonly = random_pubkeys(300);
    for key in &readonly {
        ctx.rpc_client.add_account(
            *key,
            Account {
                lamports: 1_000_000,
                owner,
                ..Default::default()
            },
        );
    }
    for batch in readonly.chunks(20) {
        ctx.chainlink
            .ensure_accounts(
                batch,
                AccountFetchContext::rpc_get_multiple_accounts(),
            )
            .await
            .unwrap();
    }

    assert_cloned_as_delegated!(ctx.bank, &[delegated]);
}
