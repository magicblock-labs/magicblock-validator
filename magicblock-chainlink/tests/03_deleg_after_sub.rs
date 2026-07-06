use magicblock_chainlink::{
    AccountFetchContext, assert_cloned_as_delegated,
    assert_cloned_as_empty_placeholder, assert_cloned_as_undelegated,
    assert_not_cloned, assert_not_subscribed,
    assert_subscribed_without_delegation_record,
    testing::{
        accounts::account_shared_with_owner_and_slot, context::TestContext,
        deleg::add_delegation_record_for,
    },
};
use solana_account::{Account, AccountBuilder, AccountMode};
use solana_pubkey::Pubkey;
use tracing::*;

// Implements the following flow:
//
// ## Account created then fetched, then delegated
// @docs/flows/deleg-non-existing-after-sub.md

// NOTE: Flow "Account created then fetched, then delegated"
#[tokio::test]
async fn test_deleg_after_subscribe_case2() {
    let mut slot: u64 = 11;

    let ctx = TestContext::init(slot).await;
    let TestContext {
        chainlink,
        bank,
        pubsub_client: _,
        rpc_client,
        ..
    } = ctx.clone();

    let pubkey = Pubkey::new_unique();
    let program_pubkey = Pubkey::new_unique();
    let acc = Account {
        lamports: 1_000_000,
        owner: program_pubkey,
        ..Default::default()
    };

    // 1. Initially the account does not exist
    // - readable: OK (non existing account)
    // - writable: NO
    {
        info!("1. Initially the account does not exist");
        assert_not_cloned!(bank, &[pubkey]);

        chainlink
            .ensure_accounts(
                &[pubkey],
                AccountFetchContext::rpc_get_multiple_accounts(),
            )
            .await
            .unwrap();
        assert_cloned_as_empty_placeholder!(bank, &[pubkey]);
        let account = bank.accounts().get(&pubkey).unwrap().unwrap();
        assert_eq!(account.mode(), AccountMode::Placeholder);
        assert_subscribed_without_delegation_record!(&chainlink, &[&pubkey]);
    }

    // 2. Account created with original owner
    //
    // The retained subscription replaces the placeholder with readonly state.
    // - readable: OK
    // - writable: NO
    {
        info!("2. Create account owned by program {program_pubkey}");

        slot = rpc_client.set_slot(slot + 11);
        let acc =
            account_shared_with_owner_and_slot(&acc, program_pubkey, slot);

        assert!(chainlink.is_watching(&pubkey));
        let expected = AccountBuilder::from(acc.clone())
            .mode(AccountMode::ReadOnly)
            .build();
        let mut local_updates = bank.accounts().subscribe(pubkey).await;
        ctx.send_account_update(pubkey, acc.clone()).await;
        TestContext::wait_for_local_account(
            &bank,
            &pubkey,
            &mut local_updates,
            &expected,
        )
        .await;
        assert_cloned_as_undelegated!(bank, &[pubkey], slot, program_pubkey);
        assert_subscribed_without_delegation_record!(&chainlink, &[&pubkey]);
    }
    // 3. Account delegated to us
    //
    // Delegate account to us and the sub update should be received
    // even before the ensure_writable request
    {
        info!("3. Delegate account to us");

        slot = rpc_client.set_slot(slot + 11);
        let acc = account_shared_with_owner_and_slot(&acc, dlp_api::id(), slot);
        let delegation_record = add_delegation_record_for(
            &rpc_client,
            pubkey,
            ctx.validator_pubkey,
            program_pubkey,
        );
        let expected = AccountBuilder::from(acc.clone())
            .owner(program_pubkey)
            .mode(AccountMode::Delegated)
            .build();
        let mut local_updates = bank.accounts().subscribe(pubkey).await;
        ctx.send_account_update(pubkey, acc.clone()).await;
        TestContext::wait_for_local_account(
            &bank,
            &pubkey,
            &mut local_updates,
            &expected,
        )
        .await;
        assert_cloned_as_delegated!(bank, &[pubkey], slot, program_pubkey);
        assert_not_subscribed!(&chainlink, &[&pubkey, &delegation_record]);
    }
}
