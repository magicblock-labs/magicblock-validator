// Implements the following flow:
//
// ## Redelegate an Account that was delegated to us to Other - Same Slot
// @docs/flows/deleg-us-redeleg-other.md

use chainlink::testing::deleg::add_delegation_record_for;
use chainlink::testing::init_logger;
use chainlink::{
    assert_cloned_as_delegated, assert_cloned_as_undelegated,
    assert_not_subscribed, assert_remain_undelegating,
    assert_subscribed_without_delegation_record,
};
use log::*;
use solana_account::Account;
use solana_sdk::clock::Slot;
use utils::accounts::account_shared_with_owner_and_slot;
use utils::test_context::TestContext;

use solana_pubkey::Pubkey;

mod utils;

async fn setup(slot: Slot) -> TestContext {
    init_logger();
    TestContext::init(slot).await
}

#[tokio::test]
async fn test_undelegate_redelegate_to_other_in_same_slot() {
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
        let delegated_acc = account_shared_with_owner_and_slot(
            &acc,
            ephemeral_rollups_sdk::id(),
            slot,
        );
        rpc_client.add_account(pubkey, delegated_acc.clone().into());
        let delegation_record = add_delegation_record_for(
            &rpc_client,
            pubkey,
            ctx.validator_pubkey,
            program_pubkey,
        );

        // Transaction to read/write would be ok
        // Fetch account - see it's owned by DP, fetch delegation record, clone account as delegated
        ctx.ensure_account(&pubkey).await.unwrap();
        assert_cloned_as_delegated!(cloner, &[pubkey], slot, program_pubkey);
        assert_not_subscribed!(&chainlink, &[&pubkey, &delegation_record]);
    };

    // 2. Account is undelegated and redelegated to another authority (same slot)
    // Undelegation requested, setup subscription, writes refused
    {
        info!("2.1. Account is undelegated - Undelegation requested (account owner set to DP in Ephem)");

        ctx.force_undelegation(&pubkey);

        info!("2.2. Would refuse write (account still owned by DP in Ephem)");
        assert_remain_undelegating!(cloner, &[pubkey], slot);

        slot = rpc_client.set_slot(slot + 1);

        info!("2.3. Account is undelegated and redelegated to other authority in same slot");

        // First trigger undelegation subscription
        ctx.chainlink.undelegation_requested(&pubkey).await.unwrap();

        // Then immediateljky delegate to other authority (simulating same slot operation)
        ctx.delegate_existing_account_to(
            &pubkey,
            &other_authority,
            &program_pubkey,
        )
        .await
        .unwrap();

        // Account should be cloned as delegated to other (flagged as undelegated)
        info!("2.4. Would refuse write (delegated to other)");
        assert_cloned_as_undelegated!(cloner, &[pubkey], slot, program_pubkey);
        assert_subscribed_without_delegation_record!(&chainlink, &[&pubkey]);
    }
}
