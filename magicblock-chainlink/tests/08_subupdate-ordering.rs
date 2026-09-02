use magicblock_chainlink::{
    AccountFetchEntrypoint,
    testing::{
        accounts::account_shared_with_owner_and_slot, context::TestContext,
    },
};
use solana_account::{Account, AccountBuilder, AccountMode, ReadableAccount};
use solana_pubkey::Pubkey;
use tracing::*;

#[tokio::test]
async fn test_subs_receive_out_of_order_updates() {
    let ctx = TestContext::init(1).await;
    let chainlink = ctx.chainlink.clone();
    let bank = ctx.bank.clone();
    let rpc_client = ctx.rpc_client.clone();

    let pubkey = Pubkey::new_unique();
    let acc_state_1 = Account {
        lamports: 1_000_000,
        data: vec![1; 10],
        ..Default::default()
    };
    let acc_state_2 = Account {
        lamports: 2_000_000,
        data: vec![2; 10],
        ..Default::default()
    };
    let acc_state_3 = Account {
        lamports: 3_000_000,
        data: vec![3; 10],
        ..Default::default()
    };
    // 1. Account exists in state 1
    rpc_client.add_account(
        pubkey,
        account_shared_with_owner_and_slot(
            &acc_state_1,
            Pubkey::new_unique(),
            1,
        )
        .into(),
    );

    chainlink
        .ensure_accounts(
            &[pubkey],
            AccountFetchEntrypoint::RpcGetMultipleAccounts,
        )
        .await
        .unwrap();

    let initial_matches = bank
        .accounts()
        .loader()
        .read(&pubkey, |account| {
            account.lamports() == 1_000_000
                && account.data() == vec![1; 10].as_slice()
        })
        .unwrap()
        .expect("Account should be cloned");
    assert!(initial_matches);

    let mut local_updates = bank.accounts().subscribe(pubkey).await;

    // 2. Simulate update 3 arriving before update 2 because the latter is slow
    rpc_client.set_slot(3);
    debug!(update_number = 3, "Sending update");
    let expected_state_3 = AccountBuilder::from(acc_state_3.clone())
        .slot(3)
        .mode(AccountMode::ReadOnly)
        .build();
    assert!(
        ctx.send_and_receive_account_update(pubkey, acc_state_3.clone(), None,)
            .await,
        "state 3 update was not processed"
    );
    TestContext::wait_for_local_account(
        &bank,
        &pubkey,
        &mut local_updates,
        &expected_state_3,
    )
    .await;

    // 3. Now update two finally arrives
    debug!(update_number = 2, delayed = true, "Sending update");
    assert!(
        ctx.send_and_receive_account_update(pubkey, acc_state_2.clone(), None,)
            .await,
        "stale state 2 update was not processed"
    );

    let final_matches = bank
        .accounts()
        .loader()
        .read(&pubkey, |account| account == &expected_state_3)
        .unwrap()
        .expect("Account should be cloned");
    assert!(final_matches);
}
