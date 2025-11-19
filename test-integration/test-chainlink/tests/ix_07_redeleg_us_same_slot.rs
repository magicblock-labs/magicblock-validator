// Implements the following flow:
//
// ## Redelegate an Account that was delegated to us to us - Same Slot
// @docs/flows/deleg-us-redeleg-us.md

use log::*;
use magicblock_chainlink::{
    assert_cloned_as_delegated, assert_not_subscribed,
    testing::{init_logger, utils::sleep_ms},
};
use solana_sdk::{signature::Keypair, signer::Signer};
use test_chainlink::ixtest_context::IxtestContext;

#[tokio::test]
async fn ixtest_undelegate_redelegate_to_us_in_same_slot() {
    init_logger();

    let ctx = IxtestContext::init().await;

    // Create and delegate a counter account to us
    let counter_auth = Keypair::new();
    ctx.init_counter(&counter_auth)
        .await
        .delegate_counter(&counter_auth)
        .await;

    let counter_pda = ctx.counter_pda(&counter_auth.pubkey());
    let deleg_record_pubkey = ctx.delegation_record_pubkey(&counter_pda);
    let pubkeys = [counter_pda];

    // 1. Account delegated to us - readable and writable
    {
        info!("1. Account delegated to us");

        ctx.chainlink.ensure_accounts(&pubkeys, None).await.unwrap();
        sleep_ms(1_500).await;

        // Account should be cloned as delegated
        let account = ctx.cloner.get_account(&counter_pda).unwrap();
        assert_cloned_as_delegated!(
            ctx.cloner,
            &[counter_pda],
            account.remote_slot(),
            program_flexi_counter::id()
        );

        // Accounts delegated to us should not be tracked via subscription
        assert_not_subscribed!(
            ctx.chainlink,
            &[&deleg_record_pubkey, &counter_pda]
        );
    }

    // 2. Account is undelegated and redelegated to us (same slot) - writes allowed again
    {
        info!(
            "2. Account is undelegated and redelegated to us in the same slot"
        );

        ctx.undelegate_counter(&counter_auth, true).await;

        // Wait for pubsub update to trigger subscription handler
        sleep_ms(1_500).await;

        // Account should still be cloned as delegated to us
        let account = ctx.cloner.get_account(&counter_pda).unwrap();
        assert_cloned_as_delegated!(
            ctx.cloner,
            &[counter_pda],
            account.remote_slot(),
            program_flexi_counter::id()
        );

        // Accounts delegated to us should not be tracked via subscription
        assert_not_subscribed!(
            ctx.chainlink,
            &[&deleg_record_pubkey, &counter_pda]
        );
    }
}
