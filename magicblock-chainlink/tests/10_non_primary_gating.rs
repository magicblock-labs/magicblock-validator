use magicblock_chainlink::{testing::init_logger, AccountFetchOrigin};
use magicblock_core::coordination_mode::{
    switch_to_primary_mode, switch_to_replica_mode,
};
use solana_account::Account;
use solana_program::clock::Slot;
use solana_pubkey::Pubkey;
use utils::{
    accounts::account_shared_with_owner_and_slot, test_context::TestContext,
};

mod utils;

struct PrimaryModeGuard;

impl PrimaryModeGuard {
    fn enter_replica() -> Self {
        switch_to_replica_mode();
        Self
    }
}

impl Drop for PrimaryModeGuard {
    fn drop(&mut self) {
        switch_to_primary_mode();
    }
}

async fn setup(slot: Slot) -> TestContext {
    init_logger();
    TestContext::init(slot).await
}

#[tokio::test]
async fn replica_mode_chainlink_apis_are_noop_or_local_only() {
    let slot = 11;
    let ctx = setup(slot).await;
    let chainlink = ctx.chainlink.clone();

    let _primary_mode_guard = PrimaryModeGuard::enter_replica();

    let ensure_pubkey = Pubkey::new_unique();
    let ensure_owner = Pubkey::new_unique();
    let ensure_account = account_shared_with_owner_and_slot(
        &Account {
            lamports: 1_000,
            ..Default::default()
        },
        ensure_owner,
        slot,
    );
    ctx.rpc_client
        .add_account(ensure_pubkey, ensure_account.into());

    let before_ensure_fetch_count = chainlink.fetch_count().unwrap_or(0);
    let ensure_result = chainlink
        .ensure_accounts(
            &[ensure_pubkey],
            None,
            AccountFetchOrigin::GetMultipleAccounts,
            None,
        )
        .await
        .unwrap();
    assert!(ensure_result.is_ok());
    assert!(ensure_result.not_found_on_chain.is_empty());
    assert!(ensure_result.missing_delegation_record.is_empty());
    assert_eq!(
        chainlink.fetch_count().unwrap_or(0),
        before_ensure_fetch_count
    );

    let local_pubkey = Pubkey::new_unique();
    let local_owner = Pubkey::new_unique();
    let local_account = account_shared_with_owner_and_slot(
        &Account {
            lamports: 2_000,
            ..Default::default()
        },
        local_owner,
        slot,
    );
    ctx.bank.insert(local_pubkey, local_account.clone());

    let remote_account = account_shared_with_owner_and_slot(
        &Account {
            lamports: 3_000,
            ..Default::default()
        },
        Pubkey::new_unique(),
        slot + 1,
    );
    ctx.rpc_client
        .add_account(local_pubkey, remote_account.into());

    let before_fetch_count = chainlink.fetch_count().unwrap_or(0);
    let fetched_accounts = chainlink
        .fetch_accounts(
            &[local_pubkey],
            AccountFetchOrigin::GetMultipleAccounts,
            None,
        )
        .await
        .unwrap();
    assert_eq!(fetched_accounts, vec![Some(local_account)]);
    assert_eq!(chainlink.fetch_count().unwrap_or(0), before_fetch_count);

    let undelegation_pubkey = Pubkey::new_unique();
    assert!(!chainlink.is_watching(&undelegation_pubkey));
    chainlink
        .undelegation_requested(undelegation_pubkey)
        .await
        .unwrap();
    assert!(!chainlink.is_watching(&undelegation_pubkey));

    // TODO(chainlink): `subscribe_account_removals` is fed by an internal
    // removed-account channel that the current `TestContext` does not expose.
    // The replica-mode eviction path is gated by the same `remote_sync_enabled`
    // check exercised above; add a direct eviction submission assertion if the
    // test utilities grow a narrow removed-account notification hook.
}
