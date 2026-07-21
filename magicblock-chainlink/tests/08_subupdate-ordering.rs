use magicblock_chainlink::{
    AccountFetchContext,
    testing::{
        accounts::account_shared_with_owner_and_slot, context::TestContext,
    },
};
use solana_account::{Account, ReadableAccount};
use solana_pubkey::Pubkey;
use tracing::*;

#[tokio::test]
async fn test_subs_receive_out_of_order_updates() {
    let ctx = TestContext::init(1).await;
    let TestContext {
        chainlink,
        bank,
        rpc_client,
        ..
    } = ctx.clone();

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
    let acc_state_4 = Account {
        lamports: 4_000_000,
        data: vec![4; 10],
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
        .clone()
        .into(),
    );

    chainlink
        .ensure_accounts(
            &[pubkey],
            None,
            AccountFetchContext::rpc_get_multiple_accounts(),
        )
        .await
        .unwrap();

    let acc = bank
        .accounts()
        .get(&pubkey)
        .unwrap()
        .expect("Account should be cloned");
    assert_eq!(acc.lamports(), 1_000_000);
    assert_eq!(acc.data(), vec![1; 10].as_slice());

    // 2. Simulate update 3 arriving before update 2 because the latter is slow
    rpc_client.set_slot(3);
    debug!(update_number = 3, "Sending update");
    ctx.send_and_receive_account_update(pubkey, acc_state_3.clone(), None)
        .await;
    let acc = bank
        .accounts()
        .get(&pubkey)
        .unwrap()
        .expect("Account should be cloned");
    assert_eq!(acc.lamports(), 3_000_000);
    assert_eq!(acc.data(), vec![3; 10].as_slice());

    // 3. Now update two finally arrives
    debug!(update_number = 2, delayed = true, "Sending update");
    ctx.send_and_receive_account_update(pubkey, acc_state_2.clone(), None)
        .await;
    let acc = bank
        .accounts()
        .get(&pubkey)
        .unwrap()
        .expect("Account should be cloned");
    // Should still be in state 3
    assert_eq!(acc.lamports(), 3_000_000);
    assert_eq!(acc.data(), vec![3; 10].as_slice());

    // 4. Finally update 4 arrives
    // This should update the account to state 4
    rpc_client.set_slot(4);
    debug!(update_number = 4, "Sending update");
    ctx.send_and_receive_account_update(pubkey, acc_state_4.clone(), None)
        .await;
    let acc = bank
        .accounts()
        .get(&pubkey)
        .unwrap()
        .expect("Account should be cloned");
    assert_eq!(acc.lamports(), 4_000_000);
    assert_eq!(acc.data(), vec![4; 10].as_slice());
}
