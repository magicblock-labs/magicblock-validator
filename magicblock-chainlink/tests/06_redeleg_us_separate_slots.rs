// Implements the following flow:
//
// ## Redelegate an Account that was delegated to us to us - Separate Slots
// @docs/flows/deleg-us-redeleg-us.md

use log::*;
use magicblock_chainlink::{
    assert_cloned_as_delegated, assert_cloned_as_undelegated,
    assert_not_subscribed, assert_remain_undelegating,
    assert_subscribed_without_delegation_record,
    testing::{
        accounts::account_shared_with_owner, deleg::add_delegation_record_for,
        init_logger,
    },
};
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_sdk::clock::Slot;
use utils::{
    accounts::account_shared_with_owner_and_slot, test_context::TestContext,
};

use crate::utils::accounts::compressed_account_shared_with_owner_and_slot;

mod utils;

async fn setup(slot: Slot) -> TestContext {
    init_logger();
    TestContext::init(slot).await
}

#[tokio::test]
async fn test_undelegate_redelegate_to_us_in_separate_slots() {
    let mut slot: u64 = 11;

    let ctx = setup(slot).await;
    let TestContext {
        chainlink,
        cloner,
        rpc_client,
        ..
    } = ctx.clone();

    let pubkey = Pubkey::new_unique();
    let program_pubkey = Pubkey::new_unique();
    let acc = Account {
        lamports: 1_000,
        ..Default::default()
    };

    // 1. Account delegated to us
    // Initial state: Account is delegated to us and we can read/write to it
    let deleg_record_pubkey = {
        info!("1. Account delegated to us");

        slot = rpc_client.set_slot(slot + 11);
        let delegated_acc =
            account_shared_with_owner_and_slot(&acc, dlp::id(), slot);

        rpc_client.add_account(pubkey, delegated_acc.clone().into());
        let delegation_record = add_delegation_record_for(
            &rpc_client,
            pubkey,
            ctx.validator_pubkey,
            program_pubkey,
        );

        // Fetch account - see it's owned by DP, fetch delegation record, clone account as delegated
        ctx.ensure_account(&pubkey).await.unwrap();
        assert_cloned_as_delegated!(cloner, &[pubkey], slot, program_pubkey);
        assert_not_subscribed!(&chainlink, &[&pubkey, &delegation_record]);

        delegation_record
    };

    // 2. Account is undelegated
    // Undelegation requested, setup subscription, writes would be refused
    {
        info!("2.1. Account is undelegated - Undelegation requested (account owner set to DP in Ephem)");

        ctx.force_undelegation(&pubkey);

        info!("2.2. Would refuse write (account still owned by DP in Ephem)");
        assert_remain_undelegating!(cloner, &[pubkey], slot);

        slot = rpc_client.set_slot(slot + 11);

        info!("2.3. Account is undelegated on chain");
        ctx.commit_and_undelegate(&pubkey, &program_pubkey)
            .await
            .unwrap();

        // Account should be cloned as undelegated
        info!("2.4. Write would be refused (undelegated on chain)");
        assert_cloned_as_undelegated!(cloner, &[pubkey], slot, program_pubkey);
        assert_subscribed_without_delegation_record!(&chainlink, &[&pubkey]);
    }

    // 3. Account redelegated to us (separate slot)
    // Delegate back to us, subscription update, writes allowed
    {
        info!("3.1. Account redelegated to us - Delegate account back to us");
        slot = rpc_client.set_slot(slot + 11);

        ctx.delegate_existing_account_to(
            &pubkey,
            &ctx.validator_pubkey,
            &program_pubkey,
        )
        .await
        .unwrap();

        // Account should be cloned as delegated back to us
        info!("3.2. Would allow write (delegated to us again)");
        assert_cloned_as_delegated!(cloner, &[pubkey], slot, program_pubkey);

        // Account is delegated to us, so we don't subscribe to it nor its delegation record
        assert_not_subscribed!(chainlink, &[&pubkey, &deleg_record_pubkey]);
    }
}

#[tokio::test]
async fn test_undelegate_redelegate_to_us_in_separate_slots_compressed() {
    let mut slot: u64 = 11;

    let ctx = setup(slot).await;
    let TestContext {
        chainlink,
        cloner,
        rpc_client,
        photon_client,
        ..
    } = ctx.clone();

    let pubkey = Pubkey::new_unique();
    let program_pubkey = Pubkey::new_unique();
    let acc = Account {
        lamports: 1_000,
        ..Default::default()
    };

    // 1. Account delegated to us
    // Initial state: Account is delegated to us and we can read/write to it
    {
        info!("1. Account delegated to us");

        slot = rpc_client.set_slot(slot + 11);
        let compressed_account = compressed_account_shared_with_owner_and_slot(
            pubkey,
            ctx.validator_pubkey,
            program_pubkey,
            slot,
        );
        photon_client.add_account(
            pubkey,
            compressed_account.clone().into(),
            slot,
        );

        let delegated_acc = account_shared_with_owner_and_slot(
            &acc,
            compressed_delegation_client::id(),
            slot,
        );
        rpc_client.add_account(pubkey, delegated_acc.clone().into());

        // Fetch account - see it's owned by DP, fetch delegation record, clone account as delegated
        ctx.ensure_account(&pubkey).await.unwrap();
        assert_cloned_as_delegated!(cloner, &[pubkey], slot, program_pubkey);
        assert_not_subscribed!(&chainlink, &[&pubkey]);
    };

    // 2. Account is undelegated
    // Undelegation requested, setup subscription, writes would be refused
    {
        info!("2.1. Account is undelegated - Undelegation requested (account owner set to DP in Ephem)");

        ctx.force_undelegation(&pubkey);

        info!("2.2. Would refuse write (account still owned by DP in Ephem)");
        assert_remain_undelegating!(cloner, &[pubkey], slot);

        slot = rpc_client.set_slot(slot + 11);

        info!("2.3. Account is undelegated on chain");
        // Committor service calls this to trigger subscription
        chainlink.undelegation_requested(pubkey).await.unwrap();

        // Committor service then requests undelegation on chain
        let acc = rpc_client.get_account_at_slot(&pubkey).unwrap();
        let undelegated_acc = account_shared_with_owner_and_slot(
            &acc.account,
            program_pubkey,
            rpc_client.get_slot(),
        );
        photon_client.add_account(pubkey, Account::default(), slot);
        let updated = ctx
            .send_and_receive_account_update(
                pubkey,
                undelegated_acc.clone(),
                Some(400),
            )
            .await;
        assert!(updated, "Failed to receive undelegation update");

        // Account should be cloned as undelegated
        info!("2.4. Write would be refused (undelegated on chain)");
        assert_cloned_as_undelegated!(cloner, &[pubkey], slot, program_pubkey);
        assert_subscribed_without_delegation_record!(&chainlink, &[&pubkey]);
    }

    // 3. Account redelegated to us (separate slot)
    // Delegate back to us, subscription update, writes allowed
    {
        info!("3.1. Account redelegated to us - Delegate account back to us");
        slot = rpc_client.set_slot(slot + 11);

        let compressed_account = compressed_account_shared_with_owner_and_slot(
            pubkey,
            ctx.validator_pubkey,
            program_pubkey,
            slot,
        );
        photon_client.add_account(
            pubkey,
            compressed_account.clone().into(),
            slot,
        );
        let delegated_acc = account_shared_with_owner(
            &Account::default(),
            compressed_delegation_client::id(),
        );
        let updated = ctx
            .send_and_receive_account_update(
                pubkey,
                delegated_acc.clone(),
                Some(400),
            )
            .await;
        assert!(updated, "Failed to receive delegation update");

        chainlink.ensure_accounts(&[pubkey], None).await.unwrap();

        // Account should be cloned as delegated back to us
        info!("3.2. Would allow write (delegated to us again)");
        assert_cloned_as_delegated!(cloner, &[pubkey], slot, program_pubkey);

        // Account is delegated to us, so we don't subscribe to it nor its delegation record
        assert_not_subscribed!(chainlink, &[&pubkey]);
    }
}
