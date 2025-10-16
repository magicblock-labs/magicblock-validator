use log::*;
use magicblock_chainlink::{
    assert_cloned_as_delegated, assert_cloned_as_empty_placeholder,
    assert_cloned_as_undelegated, assert_not_cloned, assert_not_subscribed,
    assert_subscribed, testing::init_logger,
};
use solana_sdk::{signature::Keypair, signer::Signer};
use test_chainlink::{
    accounts::{sanitized_transaction_with_accounts, TransactionAccounts},
    ixtest_context::IxtestContext,
};

#[tokio::test]
async fn ixtest_feepayer_with_delegated_ephemeral_balance() {
    init_logger();
    let payer_kp = Keypair::new();

    let ctx = IxtestContext::init().await;

    ctx.add_account(&payer_kp.pubkey(), 2).await;
    let accounts = TransactionAccounts {
        writable_accounts: vec![payer_kp.pubkey()],
        ..Default::default()
    };
    let tx = sanitized_transaction_with_accounts(&accounts);

    let (escrow_pda, escrow_deleg_record) =
        ctx.top_up_ephemeral_fee_balance(&payer_kp, 1, true).await;

    let res = ctx
        .chainlink
        .ensure_transaction_accounts(&tx)
        .await
        .unwrap();

    debug!("res: {res:?}");
    debug!("cloned accounts: {}", ctx.cloner.dump_account_keys(false));

    assert_cloned_as_undelegated!(&ctx.cloner, &[payer_kp.pubkey()]);
    assert_cloned_as_delegated!(&ctx.cloner, &[escrow_pda]);
    assert_not_cloned!(&ctx.cloner, &[escrow_deleg_record]);

    assert_subscribed!(ctx.chainlink, &[&payer_kp.pubkey()]);
    assert_not_subscribed!(ctx.chainlink, &[&escrow_pda, &escrow_deleg_record]);
}

#[tokio::test]
async fn ixtest_feepayer_with_undelegated_ephemeral_balance() {
    init_logger();
    let payer_kp = Keypair::new();

    let ctx = IxtestContext::init().await;

    ctx.add_account(&payer_kp.pubkey(), 2).await;
    let accounts = TransactionAccounts {
        writable_accounts: vec![payer_kp.pubkey()],
        ..Default::default()
    };
    let tx = sanitized_transaction_with_accounts(&accounts);

    let (escrow_pda, escrow_deleg_record) =
        ctx.top_up_ephemeral_fee_balance(&payer_kp, 1, false).await;

    let res = ctx
        .chainlink
        .ensure_transaction_accounts(&tx)
        .await
        .unwrap();

    debug!("res: {res:?}");
    debug!("cloned accounts: {}", ctx.cloner.dump_account_keys(false));

    assert_cloned_as_undelegated!(&ctx.cloner, &[payer_kp.pubkey()]);
    assert_cloned_as_undelegated!(&ctx.cloner, &[escrow_pda]);
    assert_not_cloned!(&ctx.cloner, &[escrow_deleg_record]);

    assert_subscribed!(ctx.chainlink, &[&payer_kp.pubkey(), &escrow_pda,]);
    assert_not_subscribed!(ctx.chainlink, &[&escrow_deleg_record]);
}

#[tokio::test]
async fn ixtest_feepayer_without_ephemeral_balance() {
    init_logger();
    let payer_kp = Keypair::new();

    let ctx = IxtestContext::init().await;

    ctx.add_account(&payer_kp.pubkey(), 2).await;
    let accounts = TransactionAccounts {
        writable_accounts: vec![payer_kp.pubkey()],
        ..Default::default()
    };
    let tx = sanitized_transaction_with_accounts(&accounts);

    let res = ctx
        .chainlink
        .ensure_transaction_accounts(&tx)
        .await
        .unwrap();

    debug!("res: {res:?}");
    debug!("cloned accounts: {}", ctx.cloner.dump_account_keys(false));

    let (escrow_pda, escrow_deleg_record) = ctx.escrow_pdas(&payer_kp.pubkey());

    assert_cloned_as_undelegated!(&ctx.cloner, &[payer_kp.pubkey()]);
    assert_cloned_as_empty_placeholder!(&ctx.cloner, &[escrow_pda]);
    assert_subscribed!(ctx.chainlink, &[&payer_kp.pubkey(), &escrow_pda]);

    assert_not_cloned!(&ctx.cloner, &[escrow_deleg_record]);
    assert_not_subscribed!(ctx.chainlink, &[&escrow_deleg_record]);
}

#[tokio::test]
async fn ixtest_feepayer_delegated_to_us() {
    init_logger();
    let payer_kp = Keypair::new();

    let ctx = IxtestContext::init().await;
    ctx.init_counter(&payer_kp)
        .await
        .delegate_counter(&payer_kp)
        .await;
    let counter_pda = ctx.counter_pda(&payer_kp.pubkey());

    let accounts = TransactionAccounts {
        writable_accounts: vec![counter_pda],
        ..Default::default()
    };
    // 1. Send the first transaction with the counter_pda
    let tx = sanitized_transaction_with_accounts(&accounts);

    let res = ctx
        .chainlink
        .ensure_transaction_accounts(&tx)
        .await
        .unwrap();

    debug!("res: {res:?}");
    debug!("cloned accounts: {}", ctx.cloner.dump_account_keys(false));

    let (escrow_pda, _) = ctx.escrow_pdas(&counter_pda);

    assert_cloned_as_delegated!(&ctx.cloner, &[counter_pda]);
    assert_cloned_as_empty_placeholder!(&ctx.cloner, &[escrow_pda]);
    assert_subscribed!(ctx.chainlink, &[&escrow_pda]);
    assert_not_subscribed!(ctx.chainlink, &[&counter_pda]);

    // Initially the counter_pda is not in the bank, thus we optimistically
    // try to clone its escrow and fail to find it, however we clone it as
    // an empty placeholder. Thus it is not included as not found on chain
    assert!(res.pubkeys_not_found_on_chain().is_empty());

    // 2. Send the second transaction with the counter_pda (it is now already in the bank)
    let res = ctx
        .chainlink
        .ensure_transaction_accounts(&tx)
        .await
        .unwrap();

    debug!("res: {res:?}");
    debug!("cloned accounts: {}", ctx.cloner.dump_account_keys(false));

    assert_cloned_as_delegated!(&ctx.cloner, &[counter_pda]);
    assert_cloned_as_empty_placeholder!(&ctx.cloner, &[escrow_pda]);
    assert_subscribed!(ctx.chainlink, &[&escrow_pda]);
    assert_not_subscribed!(ctx.chainlink, &[&counter_pda]);

    assert!(res.pubkeys_not_found_on_chain().is_empty());
}
