use log::*;
use magicblock_chainlink::{
    assert_cloned_as_delegated, assert_cloned_as_undelegated,
    assert_not_cloned, assert_not_found, assert_not_subscribed,
    assert_subscribed_without_delegation_record, testing::init_logger,
    AccountFetchOrigin,
};
use solana_sdk::{signature::Keypair, signer::Signer};
use test_chainlink::ixtest_context::IxtestContext;

#[tokio::test]
async fn ixtest_deleg_after_subscribe_case2() {
    init_logger();

    let ctx = IxtestContext::init().await;

    let counter_auth = Keypair::new();
    let counter_pda = ctx.counter_pda(&counter_auth.pubkey());
    let pubkeys = [counter_pda];

    // 1. Initially the account does not exist
    {
        info!("1. Initially the account does not exist");
        let res = ctx.chainlink
        .ensure_accounts(&pubkeys, None, AccountFetchOrigin::GetAccount)
        .await
        .unwrap();

        assert_not_found!(res, &pubkeys);
        assert_not_cloned!(ctx.cloner, &pubkeys);
        assert_not_subscribed!(ctx.chainlink, &[&counter_pda]);
    }

    // 2. Account created with original owner (program)
    {
        info!("2. Create account owned by program_flexi_counter");
        ctx.init_counter(&counter_auth).await;

        ctx.chainlink
        .ensure_accounts(&pubkeys, None, AccountFetchOrigin::GetAccount)
        .await
        .unwrap();

        // Assert cloned account state matches the remote account and slot
        let account = ctx.cloner.get_account(&counter_pda).unwrap();
        assert_cloned_as_undelegated!(
            ctx.cloner,
            &[counter_pda],
            account.remote_slot(),
            program_flexi_counter::id()
        );

        assert_subscribed_without_delegation_record!(ctx.chainlink, &pubkeys);
    }

    // 3. Account delegated to us
    {
        info!("3. Delegate account to us");
        ctx.delegate_counter(&counter_auth).await;

        let deleg_record_pubkey = ctx.delegation_record_pubkey(&counter_pda);

        ctx.chainlink
        .ensure_accounts(&pubkeys, None, AccountFetchOrigin::GetAccount)
        .await
        .unwrap();

        let account = ctx.cloner.get_account(&counter_pda).unwrap();
        assert_cloned_as_delegated!(
            ctx.cloner,
            &[counter_pda],
            account.remote_slot(),
            program_flexi_counter::id()
        );

        assert_not_subscribed!(
            ctx.chainlink,
            &[&deleg_record_pubkey, &counter_pda]
        );
    }
}
