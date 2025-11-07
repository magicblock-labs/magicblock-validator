// Implements the following flow:
//
// ## Redelegate an Account that was delegated to us to Other - Separate Slots
// @docs/flows/deleg-us-redeleg-other.md

use log::*;
use magicblock_chainlink::{
    assert_cloned_as_delegated, assert_cloned_as_undelegated,
    assert_not_subscribed, assert_remain_undelegating,
    assert_subscribed_without_delegation_record,
    testing::{deleg::add_delegation_record_for, init_logger},
};
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_sdk::clock::Slot;
use utils::{
    accounts::account_shared_with_owner_and_slot,
    test_context::{DelegateResult, TestContext},
};

use crate::utils::accounts::compressed_account_shared_with_owner_and_slot;

mod utils;

async fn setup(slot: Slot) -> TestContext {
    init_logger();
    TestContext::init(slot).await
}

#[tokio::test]
async fn test_undelegate_redelegate_to_other_in_separate_slot() {
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
    let other_authority = Pubkey::new_unique();
    let acc = Account {
        lamports: 1_000,
        ..Default::default()
    };

    // 1. Account delegated to us
    // Initial state: Account is delegated to us and we can read/write to it
    {
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

        // Transaction to read
        // Fetch account - see it's owned by DP, fetch delegation record, clone account as delegated
        ctx.ensure_account(&pubkey).await.unwrap();
        assert_cloned_as_delegated!(cloner, &[pubkey], slot, program_pubkey);
        assert_not_subscribed!(&chainlink, &[&pubkey, &delegation_record]);
    };

    // 2. Account is undelegated
    // Undelegation requested, setup subscription, writes refused
    {
        info!("2.1. Account is undelegated - Undelegation requested (account owner set to DP in Ephem)");

        ctx.force_undelegation(&pubkey);

        info!("2.2. Would refuse write (account still owned by DP in Ephem)");
        ctx.ensure_account(&pubkey).await.unwrap();
        assert_remain_undelegating!(cloner, &[pubkey], slot);

        slot = rpc_client.set_slot(slot + 11);

        info!("2.3. Account is undelegated on chain");
        let undelegated_acc = ctx
            .commit_and_undelegate(&pubkey, &program_pubkey)
            .await
            .unwrap();

        // Account should be cloned as undelegated
        assert_eq!(cloner.get_account(&pubkey).unwrap(), undelegated_acc);

        info!("2.4. Would refuse write (undelegated on chain)");
        ctx.ensure_account(&pubkey).await.unwrap();
        assert_cloned_as_undelegated!(cloner, &[pubkey], slot, program_pubkey);
        assert_subscribed_without_delegation_record!(&chainlink, &[&pubkey]);
    }

    // 4. Account redelegated to another authority
    // Delegate to other, subscription update, writes refused
    {
        info!("4.1. Account redelegated to another authority - Delegate account to other");
        slot = rpc_client.set_slot(slot + 2);

        let DelegateResult {
            delegated_account, ..
        } = ctx
            .delegate_existing_account_to(
                &pubkey,
                &other_authority,
                &program_pubkey,
            )
            .await
            .unwrap();

        // Account should remain owned by DP but delegated to other authority
        let acc_redeleg_expected = account_shared_with_owner_and_slot(
            &delegated_account.into(),
            program_pubkey,
            slot,
        );
        assert_eq!(cloner.get_account(&pubkey).unwrap(), acc_redeleg_expected);

        info!("4.2. Would refuse write (delegated to other)");
        ctx.ensure_account(&pubkey).await.unwrap();
        assert_cloned_as_undelegated!(cloner, &[pubkey], slot, program_pubkey);
        assert_subscribed_without_delegation_record!(&chainlink, &[&pubkey]);
    }
}

#[tokio::test]
async fn test_undelegate_redelegate_to_other_in_separate_slot_compressed() {
    let mut slot: u64 = 11;

    let ctx = setup(slot).await;
    let TestContext {
        chainlink,
        cloner,
        rpc_client,
        photon_client,
        validator_pubkey,
        ..
    } = ctx.clone();

    let pubkey = Pubkey::new_unique();
    let program_pubkey = Pubkey::new_unique();
    let other_authority = Pubkey::new_unique();
    let acc = Account::default();

    // 1. Account delegated to us
    // Initial state: Account is delegated to us and we can read/write to it
    {
        info!("1. Account delegated to us");

        slot = rpc_client.set_slot(slot + 11);
        let mut compressed_account =
            compressed_account_shared_with_owner_and_slot(
                pubkey,
                validator_pubkey,
                program_pubkey,
                slot,
            );
        compressed_account.set_remote_slot(slot);
        rpc_client.add_account(pubkey, acc.clone());
        photon_client.add_account(pubkey, compressed_account.into(), slot);

        // Transaction to read
        // Fetch account - see it's owned by DP, fetch compressed account, clone account as delegated
        ctx.ensure_account(&pubkey).await.unwrap();
        assert_cloned_as_delegated!(cloner, &[pubkey], slot, program_pubkey);
        assert_not_subscribed!(&chainlink, &[&pubkey]);
    };

    // 2. Account is undelegated
    // Undelegation requested, setup subscription, writes refused
    {
        info!("2.1. Account is undelegated - Undelegation requested (account owner set to DP in Ephem)");

        ctx.force_undelegation(&pubkey);

        info!("2.2. Would refuse write (account still owned by DP in Ephem)");
        ctx.ensure_account(&pubkey).await.unwrap();
        assert_remain_undelegating!(cloner, &[pubkey], slot);

        slot = rpc_client.set_slot(slot + 11);

        info!("2.3. Account is undelegated on chain");
        let undelegated_acc = ctx
            .commit_and_undelegate(&pubkey, &program_pubkey)
            .await
            .unwrap();

        // Account should be cloned as undelegated
        assert_eq!(cloner.get_account(&pubkey).unwrap(), undelegated_acc);

        info!("2.4. Would refuse write (undelegated on chain)");
        ctx.ensure_account(&pubkey).await.unwrap();
        assert_cloned_as_undelegated!(cloner, &[pubkey], slot, program_pubkey);
        assert_subscribed_without_delegation_record!(&chainlink, &[&pubkey]);
    }

    // 4. Account redelegated to another authority
    // Delegate to other, subscription update, writes refused
    {
        info!("4.1. Account redelegated to another authority - Delegate account to other");
        slot = rpc_client.set_slot(slot + 2);

        // Create compressed delegation record
        let compressed_account = compressed_account_shared_with_owner_and_slot(
            pubkey,
            other_authority,
            program_pubkey,
            slot,
        );
        photon_client.add_account(
            pubkey,
            compressed_account.clone().into(),
            slot,
        );

        let acc = rpc_client.get_account_at_slot(&pubkey).unwrap();
        let updated = ctx
            .send_and_receive_account_update(
                pubkey,
                acc.account.clone(),
                Some(400),
            )
            .await;
        assert!(updated, "Failed to receive delegation update");

        // Account should remain owned by DP but delegated to other authority
        let acc_redeleg_expected = account_shared_with_owner_and_slot(
            &acc.account,
            program_pubkey,
            slot,
        );
        assert_eq!(cloner.get_account(&pubkey).unwrap(), acc_redeleg_expected);

        info!("4.2. Would refuse write (delegated to other)");
        ctx.ensure_account(&pubkey).await.unwrap();
        assert_cloned_as_undelegated!(cloner, &[pubkey], slot, program_pubkey);
        assert_subscribed_without_delegation_record!(&chainlink, &[&pubkey]);
    }
}
