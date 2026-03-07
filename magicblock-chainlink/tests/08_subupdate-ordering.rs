use magicblock_chainlink::{testing::init_logger, AccountFetchOrigin};
use solana_account::{Account, ReadableAccount};
use solana_program::clock::Slot;
use solana_pubkey::Pubkey;
use tracing::*;
use utils::{
    accounts::account_shared_with_owner_and_slot, test_context::TestContext,
};
async fn setup(slot: Slot) -> TestContext {
    init_logger();
    TestContext::init(slot).await
}
mod utils;

#[tokio::test]
async fn test_subs_receive_out_of_order_updates() {
    let ctx = setup(1).await;
    let TestContext {
        chainlink,
        cloner,
        rpc_client,
        ..
    } = ctx.clone();

    let pubkey = Pubkey::new_unique();
    let acc_state_1 = Account {
        lamports: 1_000,
        data: vec![1; 10],
        ..Default::default()
    };
    let acc_state_2 = Account {
        lamports: 2_000,
        data: vec![2; 10],
        ..Default::default()
    };
    let acc_state_3 = Account {
        lamports: 3_000,
        data: vec![3; 10],
        ..Default::default()
    };
    let acc_state_4 = Account {
        lamports: 4_000,
        data: vec![4; 10],
        ..Default::default()
    };
    let acc_state_5 = Account {
        lamports: 5_000,
        data: vec![5; 10],
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
            AccountFetchOrigin::GetMultipleAccounts,
            None,
        )
        .await
        .unwrap();

    let acc = cloner
        .get_account(&pubkey)
        .expect("Account should be cloned");
    assert_eq!(acc.lamports(), 1_000);
    assert_eq!(acc.data(), vec![1; 10].as_slice());

    // 2. Simulate update 3 arriving before update 2 because the latter is slow
    rpc_client.set_slot(3);
    debug!(update_number = 3, "Sending update");
    ctx.send_and_receive_account_update(pubkey, acc_state_3.clone(), None)
        .await;
    let acc = cloner
        .get_account(&pubkey)
        .expect("Account should be cloned");
    assert_eq!(acc.lamports(), 3_000);
    assert_eq!(acc.data(), vec![3; 10].as_slice());

    // 3. Now update two finally arrives (but with same slot as current state)
    // This should be accepted now since we allow same slot updates
    debug!(update_number = 2, delayed = true, "Sending update");
    ctx.send_and_receive_account_update(pubkey, acc_state_2.clone(), None)
        .await;
    let acc = cloner
        .get_account(&pubkey)
        .expect("Account should be cloned");
    // Now accepts same slot updates, so should update to state 2
    assert_eq!(acc.lamports(), 2_000);
    assert_eq!(acc.data(), vec![2; 10].as_slice());

    // 4. Finally update 4 arrives
    // This should update the account to state 4
    rpc_client.set_slot(4);
    debug!(update_number = 4, "Sending update");
    ctx.send_and_receive_account_update(pubkey, acc_state_4.clone(), None)
        .await;
    let acc = cloner
        .get_account(&pubkey)
        .expect("Account should be cloned");
    assert_eq!(acc.lamports(), 4_000);
    assert_eq!(acc.data(), vec![4; 10].as_slice());

    // 5. Now send an update from a lower slot (state 5 at slot 2)
    // This should be rejected since we have a newer slot (4)
    rpc_client.set_slot(2);
    debug!(
        update_number = 5,
        slot = 2,
        "Sending update from lower slot"
    );
    ctx.send_and_receive_account_update(pubkey, acc_state_5.clone(), None)
        .await;
    let acc = cloner
        .get_account(&pubkey)
        .expect("Account should be cloned");
    // Should still be in state 4, not state 5
    assert_eq!(acc.lamports(), 4_000);
    assert_eq!(acc.data(), vec![4; 10].as_slice());
}
