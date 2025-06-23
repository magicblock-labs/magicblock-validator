use std::{collections::HashSet, sync::Arc};

use conjunto_transwise::{
    transaction_accounts_extractor::TransactionAccountsExtractorImpl,
    transaction_accounts_holder::TransactionAccountsHolder,
    transaction_accounts_validator::TransactionAccountsValidatorImpl,
};
use magicblock_account_cloner::{
    AccountCloner, RemoteAccountClonerClient, RemoteAccountClonerWorker,
    ValidatorCollectionMode,
};
use magicblock_account_dumper::AccountDumperStub;
use magicblock_account_fetcher::AccountFetcherStub;
use magicblock_account_updates::AccountUpdatesStub;
use magicblock_accounts::{
    errors::AccountsError, ExternalAccountsManager, LifecycleMode,
};
use magicblock_accounts_api::InternalAccountProviderStub;
use solana_sdk::pubkey::Pubkey;
use stubs::{
    account_committer_stub::AccountCommitterStub,
    scheduled_commits_processor_stub::ScheduledCommitsProcessorStub,
};
use test_tools_core::init_logger;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

mod stubs;

type StubbedAccountsManager = ExternalAccountsManager<
    InternalAccountProviderStub,
    RemoteAccountClonerClient,
    AccountCommitterStub,
    TransactionAccountsExtractorImpl,
    TransactionAccountsValidatorImpl,
    ScheduledCommitsProcessorStub,
>;

fn setup_with_lifecycle(
    internal_account_provider: InternalAccountProviderStub,
    account_fetcher: AccountFetcherStub,
    account_updates: AccountUpdatesStub,
    account_dumper: AccountDumperStub,
    lifecycle: LifecycleMode,
) -> (StubbedAccountsManager, CancellationToken, JoinHandle<()>) {
    let cancellation_token = CancellationToken::new();

    let remote_account_cloner_worker = RemoteAccountClonerWorker::new(
        internal_account_provider.clone(),
        account_fetcher,
        account_updates,
        account_dumper,
        None,
        HashSet::new(),
        Some(1_000_000_000),
        ValidatorCollectionMode::NoFees,
        lifecycle.to_account_cloner_permissions(),
        Pubkey::new_unique(),
        1024,
    );
    let remote_account_cloner_client =
        RemoteAccountClonerClient::new(&remote_account_cloner_worker);
    let remote_account_cloner_worker_handle = {
        let cloner_cancellation_token = cancellation_token.clone();
        tokio::spawn(
            remote_account_cloner_worker
                .start_clone_request_processing(cloner_cancellation_token),
        )
    };

    let external_account_manager = ExternalAccountsManager {
        internal_account_provider,
        account_cloner: remote_account_cloner_client,
        account_committer: Arc::new(AccountCommitterStub::default()),
        transaction_accounts_extractor: TransactionAccountsExtractorImpl,
        transaction_accounts_validator: TransactionAccountsValidatorImpl,
        scheduled_commits_processor: ScheduledCommitsProcessorStub::default(),
        lifecycle,
        external_commitable_accounts: Default::default(),
    };
    (
        external_account_manager,
        cancellation_token,
        remote_account_cloner_worker_handle,
    )
}

fn setup_ephem(
    internal_account_provider: InternalAccountProviderStub,
    account_fetcher: AccountFetcherStub,
    account_updates: AccountUpdatesStub,
    account_dumper: AccountDumperStub,
) -> (StubbedAccountsManager, CancellationToken, JoinHandle<()>) {
    setup_with_lifecycle(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
        LifecycleMode::Ephemeral,
    )
}

#[tokio::test]
async fn test_ensure_readonly_account_not_tracked_nor_in_our_validator() {
    init_logger!();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );

    // Account should be fetchable but not delegated
    let undelegated_account = Pubkey::new_unique();
    account_updates.set_first_subscribed_slot(undelegated_account, 41);
    account_fetcher.set_undelegated_account(undelegated_account, 42);

    // Ensure accounts
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![undelegated_account],
                writable: vec![],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Check proper behaviour
    assert!(
        account_dumper.was_dumped_as_undelegated_account(&undelegated_account)
    );
    assert!(manager.last_commit(&undelegated_account).is_none());

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_readonly_account_not_tracked_but_in_our_validator() {
    init_logger!();
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );

    // Account should be already in the bank
    let already_loaded_account = Pubkey::new_unique();
    internal_account_provider.set(already_loaded_account, Default::default());

    // Ensure accounts
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![already_loaded_account],
                writable: vec![],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Check proper behaviour
    assert!(account_dumper.was_untouched(&already_loaded_account));
    assert_eq!(manager.last_commit(&already_loaded_account), None);

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_readonly_account_cloned_but_not_in_our_validator() {
    init_logger!();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );

    // Pre-clone the account
    let undelegated_account = Pubkey::new_unique();
    account_updates.set_first_subscribed_slot(undelegated_account, 41);
    account_fetcher.set_undelegated_account(undelegated_account, 42);
    assert!(manager
        .account_cloner
        .clone_account(&undelegated_account)
        .await
        .is_ok());
    assert!(
        account_dumper.was_dumped_as_undelegated_account(&undelegated_account)
    );
    account_dumper.clear_history();

    // Ensure accounts
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![undelegated_account],
                writable: vec![],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Check proper behaviour
    assert!(account_dumper.was_untouched(&undelegated_account));
    assert!(manager.last_commit(&undelegated_account).is_none());

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_readonly_account_cloned_but_has_been_updated_on_chain() {
    init_logger!();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );

    // Pre-clone account
    let undelegated_account = Pubkey::new_unique();
    account_updates.set_first_subscribed_slot(undelegated_account, 41);
    account_fetcher.set_undelegated_account(undelegated_account, 42);
    assert!(manager
        .account_cloner
        .clone_account(&undelegated_account)
        .await
        .is_ok());
    assert!(
        account_dumper.was_dumped_as_undelegated_account(&undelegated_account)
    );
    account_dumper.clear_history();

    // Make the account re-fetchable at a later slot with a pending update
    account_updates.set_last_known_update_slot(undelegated_account, 55);
    account_fetcher.set_undelegated_account(undelegated_account, 55);

    // Ensure accounts
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![undelegated_account],
                writable: vec![],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Check proper behaviour
    assert!(
        account_dumper.was_dumped_as_undelegated_account(&undelegated_account)
    );
    assert!(manager.last_commit(&undelegated_account).is_none());

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_readonly_account_cloned_and_no_recent_update_on_chain() {
    init_logger!();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );

    // Pre-clone the account
    let undelegated_account = Pubkey::new_unique();
    account_updates.set_first_subscribed_slot(undelegated_account, 10);
    account_fetcher.set_undelegated_account(undelegated_account, 11);
    assert!(manager
        .account_cloner
        .clone_account(&undelegated_account)
        .await
        .is_ok());
    assert!(
        account_dumper.was_dumped_as_undelegated_account(&undelegated_account)
    );
    account_dumper.clear_history();

    // Account was updated, but before the last clone's slot
    account_updates.set_last_known_update_slot(undelegated_account, 5);

    // Ensure accounts
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![undelegated_account],
                writable: vec![],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Check proper behaviour
    assert!(account_dumper.was_untouched(&undelegated_account));
    assert!(manager.last_commit(&undelegated_account).is_none());

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_readonly_account_in_our_validator_and_unseen_writable() {
    init_logger!();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );

    // One already loaded, and one properly delegated
    let already_loaded_account = Pubkey::new_unique();
    let delegated_account = Pubkey::new_unique();
    internal_account_provider.set(already_loaded_account, Default::default());
    account_updates.set_first_subscribed_slot(delegated_account, 41);
    account_fetcher.set_delegated_account(delegated_account, 42, 11);

    // Ensure accounts
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![already_loaded_account],
                writable: vec![delegated_account],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Check proper behaviour
    assert!(account_dumper.was_untouched(&already_loaded_account));
    assert!(manager.last_commit(&already_loaded_account).is_none());

    assert!(account_dumper.was_dumped_as_delegated_account(&delegated_account));
    assert!(manager.last_commit(&delegated_account).is_some());

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_one_delegated_and_one_feepayer_account_writable() {
    init_logger!();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();

    // Note: since we use a writable new account, we need to allow it as part of the configuration
    // We can't use an ephemeral's configuration, that forbids new accounts to be writable
    let (manager, cancel, handle) = setup_with_lifecycle(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        LifecycleMode::Replica,
    );

    // One writable delegated and one feepayer account
    let delegated_account = Pubkey::new_unique();
    let feepayer_account = Pubkey::new_unique();
    account_updates.set_first_subscribed_slot(delegated_account, 41);
    account_updates.set_first_subscribed_slot(feepayer_account, 41);
    account_fetcher.set_delegated_account(delegated_account, 42, 11);
    account_fetcher.set_feepayer_account(feepayer_account, 42);

    // Ensure account
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![],
                writable: vec![feepayer_account, delegated_account],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Check proper behaviour
    assert!(account_dumper.was_dumped_as_delegated_account(&delegated_account));
    assert!(manager.last_commit(&delegated_account).is_some());

    assert!(account_dumper.was_dumped_as_feepayer_account(&feepayer_account));
    assert!(manager.last_commit(&feepayer_account).is_none());

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_multiple_accounts_coming_in_over_time() {
    init_logger!();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );

    // Multiple delegated and undelegated accounts fetchable
    let undelegated_account1 = Pubkey::new_unique();
    let undelegated_account2 = Pubkey::new_unique();
    let undelegated_account3 = Pubkey::new_unique();
    let delegated_account1 = Pubkey::new_unique();
    let delegated_account2 = Pubkey::new_unique();

    account_updates.set_first_subscribed_slot(undelegated_account1, 41);
    account_updates.set_first_subscribed_slot(undelegated_account2, 41);
    account_updates.set_first_subscribed_slot(undelegated_account3, 41);
    account_updates.set_first_subscribed_slot(delegated_account1, 41);
    account_updates.set_first_subscribed_slot(delegated_account2, 41);

    account_fetcher.set_undelegated_account(undelegated_account1, 42);
    account_fetcher.set_undelegated_account(undelegated_account2, 42);
    account_fetcher.set_undelegated_account(undelegated_account3, 42);
    account_fetcher.set_delegated_account(delegated_account1, 42, 11);
    account_fetcher.set_delegated_account(delegated_account2, 42, 11);

    // First Transaction
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![undelegated_account1, undelegated_account2],
                    writable: vec![delegated_account1],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper
            .was_dumped_as_undelegated_account(&undelegated_account1));
        assert!(manager.last_commit(&undelegated_account1).is_none());

        assert!(account_dumper
            .was_dumped_as_undelegated_account(&undelegated_account2));
        assert!(manager.last_commit(&undelegated_account2).is_none());

        assert!(account_dumper.was_untouched(&undelegated_account3));
        assert!(manager.last_commit(&undelegated_account3).is_none());

        assert!(
            account_dumper.was_dumped_as_delegated_account(&delegated_account1)
        );
        assert!(manager.last_commit(&delegated_account1).is_some());

        assert!(account_dumper.was_untouched(&delegated_account2));
        assert!(manager.last_commit(&delegated_account2).is_none());
    }

    account_dumper.clear_history();

    // Second Transaction
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![undelegated_account1, undelegated_account2],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper.was_untouched(&undelegated_account1));
        assert!(manager.last_commit(&undelegated_account1).is_none());

        assert!(account_dumper.was_untouched(&undelegated_account2));
        assert!(manager.last_commit(&undelegated_account2).is_none());

        assert!(account_dumper.was_untouched(&undelegated_account3));
        assert!(manager.last_commit(&undelegated_account3).is_none());

        assert!(account_dumper.was_untouched(&delegated_account1));
        assert!(manager.last_commit(&delegated_account1).is_some());

        assert!(account_dumper.was_untouched(&delegated_account2));
        assert!(manager.last_commit(&delegated_account2).is_none());
    }

    account_dumper.clear_history();

    // Third Transaction
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![undelegated_account2, undelegated_account3],
                    writable: vec![delegated_account2],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper.was_untouched(&undelegated_account1));
        assert!(manager.last_commit(&undelegated_account1).is_none());

        assert!(account_dumper.was_untouched(&undelegated_account2));
        assert!(manager.last_commit(&undelegated_account2).is_none());

        assert!(account_dumper
            .was_dumped_as_undelegated_account(&undelegated_account3));
        assert!(manager.last_commit(&undelegated_account3).is_none());

        assert!(account_dumper.was_untouched(&delegated_account1));
        assert!(manager.last_commit(&delegated_account1).is_some());

        assert!(
            account_dumper.was_dumped_as_delegated_account(&delegated_account2)
        );
        assert!(manager.last_commit(&delegated_account2).is_some());
    }

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_accounts_seen_as_readonly_can_be_used_as_writable_later() {
    init_logger!();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );

    // A delegated account
    let delegated_account = Pubkey::new_unique();
    account_updates.set_first_subscribed_slot(delegated_account, 41);
    account_fetcher.set_delegated_account(delegated_account, 42, 11);

    // First Transaction uses the account as a readable (it should still be detected as a delegated)
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![delegated_account],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(
            account_dumper.was_dumped_as_delegated_account(&delegated_account)
        );
        assert!(manager.last_commit(&delegated_account).is_some());
    }

    account_dumper.clear_history();

    // Second Transaction uses the same account as a writable, nothing should happen
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![],
                    writable: vec![delegated_account],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper.was_untouched(&delegated_account));
        assert!(manager.last_commit(&delegated_account).is_some());
    }

    account_dumper.clear_history();

    // Third transaction reuse the account as readable, nothing should happen then
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![delegated_account],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper.was_untouched(&delegated_account));
        assert!(manager.last_commit(&delegated_account).is_some());
    }

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_accounts_already_known_can_be_reused_as_writable_later() {
    init_logger!();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );

    // Account already loaded in the bank, but is a delegated on-chain
    let delegated_account = Pubkey::new_unique();
    internal_account_provider.set(delegated_account, Default::default());
    account_updates.set_first_subscribed_slot(delegated_account, 41);
    account_fetcher.set_delegated_account(delegated_account, 42, 11);

    // First Transaction should not clone the account and use it as readonly
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![delegated_account],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper.was_untouched(&delegated_account));
        assert!(manager.last_commit(&delegated_account).is_none());
    }

    account_dumper.clear_history();

    // Second Transaction trying to use it as a writable should fail because of a local override
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![],
                    writable: vec![delegated_account],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;

        // Check proper behaviour
        assert!(matches!(
            result,
            Err(
                AccountsError::UnclonableAccountUsedAsWritableInEphemeral { .. }
            )
        ));
    }

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_accounts_already_ensured_needs_reclone_after_updates() {
    init_logger!();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );

    // Pre-clone account
    let undelegated_account = Pubkey::new_unique();
    account_updates.set_first_subscribed_slot(undelegated_account, 41);
    account_fetcher.set_undelegated_account(undelegated_account, 42);
    assert!(manager
        .account_cloner
        .clone_account(&undelegated_account)
        .await
        .is_ok());
    assert!(
        account_dumper.was_dumped_as_undelegated_account(&undelegated_account)
    );
    account_dumper.clear_history();

    // We detect an update that's more recent
    account_updates.set_last_known_update_slot(undelegated_account, 88);

    // But for this case, the account fetcher is too slow and can only fetch an old version for some reason
    account_fetcher.set_undelegated_account(undelegated_account, 77);

    // The first transaction should need to clone since there was an update
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![undelegated_account],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper
            .was_dumped_as_undelegated_account(&undelegated_account));
        assert!(manager.last_commit(&undelegated_account).is_none());
    }

    account_dumper.clear_history();

    // The second transaction should also need to clone because the previous version we cloned was too old
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![undelegated_account],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper
            .was_dumped_as_undelegated_account(&undelegated_account));
        assert!(manager.last_commit(&undelegated_account).is_none());
    }

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_accounts_already_cloned_can_be_reused_without_updates() {
    init_logger!();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );

    // Pre-clone the account
    let undelegated_account = Pubkey::new_unique();
    account_updates.set_first_subscribed_slot(undelegated_account, 41);
    account_fetcher.set_undelegated_account(undelegated_account, 42);
    assert!(manager
        .account_cloner
        .clone_account(&undelegated_account)
        .await
        .is_ok());
    assert!(
        account_dumper.was_dumped_as_undelegated_account(&undelegated_account)
    );
    account_dumper.clear_history();

    // The account has been updated on-chain since the last clone
    account_fetcher.set_undelegated_account(undelegated_account, 66);
    account_updates.set_last_known_update_slot(undelegated_account, 66);

    // The first transaction should need to clone since the account was updated on-chain since the last clone
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![undelegated_account],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper
            .was_dumped_as_undelegated_account(&undelegated_account));
        assert!(manager.last_commit(&undelegated_account).is_none());
    }

    account_dumper.clear_history();

    // The second transaction should not need to clone since the account was not updated since the first transaction's clone
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![undelegated_account],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper.was_untouched(&undelegated_account));
        assert!(manager.last_commit(&undelegated_account).is_none());
    }

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}
