use log::*;
use magicblock_chainlink::{
    assert_cloned_as_delegated, assert_cloned_as_undelegated,
    assert_not_cloned, assert_not_subscribed,
    assert_subscribed_without_delegation_record,
    testing::{deleg::add_delegation_record_for, init_logger},
    AccountFetchOrigin,
};
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_sdk::clock::Slot;
use utils::{
    accounts::account_shared_with_owner_and_slot, test_context::TestContext,
};

mod utils;

// Implements the following flow:
//
// ## Account created then fetched, then delegated
// @docs/flows/deleg-non-existing-after-sub.md

async fn setup(slot: Slot) -> TestContext {
    init_logger();
    TestContext::init(slot).await
}

// NOTE: Flow "Account created then fetched, then delegated"
#[tokio::test]
async fn test_deleg_after_subscribe_case2() {
    let mut slot: u64 = 11;

    let ctx = setup(slot).await;
    let TestContext {
        chainlink,
        cloner,
        pubsub_client: _,
        rpc_client,
        ..
    } = ctx.clone();

    let pubkey = Pubkey::new_unique();
    let program_pubkey = Pubkey::new_unique();
    let acc = Account {
        lamports: 1_000,
        owner: program_pubkey,
        ..Default::default()
    };

    // 1. Initially the account does not exist
    // - readable: OK (non existing account)
    // - writable: NO
    {
        info!("1. Initially the account does not exist");
        assert_not_cloned!(cloner, &[pubkey]);

        chainlink
            .ensure_accounts(
                &[pubkey],
                None,
                AccountFetchOrigin::GetMultipleAccounts,
            )
            .await
            .unwrap();
        assert_not_cloned!(cloner, &[pubkey]);
    }

    // 2. Account created with original owner
    //
    // Now we can ensure it as readonly and it will be cloned
    // - readable: OK
    // - writable: NO
    {
        info!("2. Create account owned by program {program_pubkey}");

        slot = rpc_client.set_slot(slot + 11);
        let acc =
            account_shared_with_owner_and_slot(&acc, program_pubkey, slot);

        // When the account is created we do not receive any update since we do not sub to a non-existing account
        let updated = ctx
            .send_and_receive_account_update(pubkey, acc.clone(), Some(400))
            .await;
        assert!(!updated);

        chainlink
            .ensure_accounts(
                &[pubkey],
                None,
                AccountFetchOrigin::GetMultipleAccounts,
            )
            .await
            .unwrap();
        assert_cloned_as_undelegated!(cloner, &[pubkey], slot, program_pubkey);
        assert_subscribed_without_delegation_record!(&chainlink, &[&pubkey]);
    }
    // 3. Account delegated to us
    //
    // Delegate account to us and the sub update should be received
    // even before the ensure_writable request
    {
        info!("3. Delegate account to us");

        slot = rpc_client.set_slot(slot + 11);
        let acc = account_shared_with_owner_and_slot(&acc, dlp::id(), slot);
        let delegation_record = add_delegation_record_for(
            &rpc_client,
            pubkey,
            ctx.validator_pubkey,
            program_pubkey,
        );
        let updated = ctx
            .send_and_receive_account_update(pubkey, acc.clone(), Some(400))
            .await;
        assert!(updated);
        assert_cloned_as_delegated!(cloner, &[pubkey], slot, program_pubkey);
        assert_not_subscribed!(&chainlink, &[&pubkey, &delegation_record]);
    }
}
