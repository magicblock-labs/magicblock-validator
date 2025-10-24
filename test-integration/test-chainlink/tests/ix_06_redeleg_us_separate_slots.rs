// Implements the following flow:
//
// ## Redelegate an Account that was delegated to us to us - Separate Slots
// @docs/flows/deleg-us-redeleg-us.md

use log::*;
use magicblock_chainlink::{
    assert_cloned_as_delegated, assert_cloned_as_undelegated,
    assert_not_subscribed, assert_subscribed_without_delegation_record,
    testing::init_logger,
};
use solana_sdk::{signature::Keypair, signer::Signer};
use test_chainlink::{ixtest_context::IxtestContext, sleep_ms};

#[tokio::test]
async fn ixtest_undelegate_redelegate_to_us_in_separate_slots() {
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

    // 2. Account is undelegated - writes refused, subscription set
    {
        info!(
            "2. Account is undelegated - Would refuse write (undelegated on chain)"
        );

        ctx.undelegate_counter(&counter_auth, false).await;

        // Account should be cloned as undelegated (owned by program again)
        let account = ctx.cloner.get_account(&counter_pda).unwrap();
        assert_cloned_as_undelegated!(
            ctx.cloner,
            &[counter_pda],
            account.remote_slot(),
            program_flexi_counter::id()
        );

        assert_subscribed_without_delegation_record!(ctx.chainlink, &pubkeys);
    }

    // 3. Account redelegated to us (separate slot) - writes allowed again
    {
        info!("3. Account redelegated to us - Would allow write");
        ctx.delegate_counter(&counter_auth).await;
        sleep_ms(500).await;

        // Account should be cloned as delegated back to us
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
