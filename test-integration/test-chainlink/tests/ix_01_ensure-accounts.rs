use log::*;
use magicblock_chainlink::{
    assert_cloned_as_delegated, assert_cloned_as_undelegated,
    assert_not_cloned, assert_not_found, assert_not_subscribed,
    assert_subscribed_without_delegation_record,
    testing::{init_logger, utils::random_pubkey},
};
use solana_sdk::{signature::Keypair, signer::Signer};
use test_chainlink::ixtest_context::IxtestContext;

#[tokio::test]
async fn ixtest_write_non_existing_account() {
    init_logger();

    let ctx = IxtestContext::init().await;

    let pubkey = random_pubkey();
    let pubkeys = [pubkey];
    let res = ctx.chainlink.ensure_accounts(&pubkeys).await.unwrap();
    debug!("res: {res:?}");

    assert_not_found!(res, &pubkeys);
    assert_not_cloned!(ctx.cloner, &pubkeys);
    assert_not_subscribed!(ctx.chainlink, &pubkeys);
}

// -----------------
// BasicScenarios:Case 1 Account is initialized and never delegated
// -----------------
#[tokio::test]
async fn ixtest_write_existing_account_undelegated() {
    init_logger();

    let ctx = IxtestContext::init().await;

    let counter_auth = Keypair::new();
    ctx.init_counter(&counter_auth).await;

    let pubkeys = [counter_auth.pubkey()];
    let res = ctx.chainlink.ensure_accounts(&pubkeys).await.unwrap();
    debug!("res: {res:?}");

    assert_cloned_as_undelegated!(ctx.cloner, &pubkeys);
    assert_subscribed_without_delegation_record!(ctx.chainlink, &pubkeys);
}

// -----------------
// BasicScenarios:Case 2 Account is initialized and already delegated to us
// -----------------
#[tokio::test]
async fn ixtest_write_existing_account_valid_delegation_record() {
    init_logger();

    let ctx = IxtestContext::init().await;

    let counter_auth = Keypair::new();
    ctx.init_counter(&counter_auth)
        .await
        .delegate_counter(&counter_auth)
        .await;

    let counter_pda = ctx.counter_pda(&counter_auth.pubkey());
    let deleg_record_pubkey = ctx.delegation_record_pubkey(&counter_pda);
    let pubkeys = [counter_pda];

    let res = ctx.chainlink.ensure_accounts(&pubkeys).await.unwrap();
    debug!("res: {res:?}");

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

// TODO(thlorenz): @ implement this test when we can actually delegate to a specific
// authority: test_write_existing_account_other_authority
