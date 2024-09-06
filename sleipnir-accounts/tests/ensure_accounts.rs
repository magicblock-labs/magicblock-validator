use std::{collections::HashSet, sync::Arc};

use conjunto_transwise::{
    errors::TranswiseError,
    transaction_accounts_extractor::TransactionAccountsExtractorImpl,
    transaction_accounts_holder::TransactionAccountsHolder,
    transaction_accounts_validator::TransactionAccountsValidatorImpl,
};
use sleipnir_account_cloner::{
    AccountCloner, RemoteAccountClonerClient, RemoteAccountClonerWorker,
};
use sleipnir_account_dumper::AccountDumperStub;
use sleipnir_account_fetcher::AccountFetcherStub;
use sleipnir_account_updates::AccountUpdatesStub;
use sleipnir_accounts::{
    errors::AccountsError, ExternalAccountsManager, LifecycleMode,
};
use sleipnir_accounts_api::InternalAccountProviderStub;
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

    let mut remote_account_cloner_worker = RemoteAccountClonerWorker::new(
        internal_account_provider.clone(),
        account_fetcher,
        account_updates,
        account_dumper,
        HashSet::new(),
        Some(1_000_000_000),
        lifecycle.allow_cloning_new_accounts(),
        lifecycle.allow_cloning_system_accounts(),
        lifecycle.allow_cloning_pda_accounts(),
        lifecycle.allow_cloning_delegated_accounts(),
        lifecycle.allow_cloning_program_accounts(),
    );
    let remote_account_cloner_client =
        RemoteAccountClonerClient::new(&remote_account_cloner_worker);
    let remote_account_cloner_worker_handle = {
        let cloner_cancellation_token = cancellation_token.clone();
        tokio::spawn(async move {
            remote_account_cloner_worker
                .start_clone_request_processing(cloner_cancellation_token)
                .await
        })
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
    let readonly_undelegated = Pubkey::new_unique();

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

    // Account should be fetchable as undelegated
    account_fetcher.set_pda_account(readonly_undelegated, 42);

    // Ensure accounts
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![readonly_undelegated],
                writable: vec![],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Check proper behaviour
    assert!(account_dumper.was_dumped_as_pda_account(&readonly_undelegated));
    assert!(manager.last_commit(&readonly_undelegated).is_none());

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_readonly_account_not_tracked_but_in_our_validator() {
    init_logger!();
    let readonly_already_loaded = Pubkey::new_unique();

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
    internal_account_provider.set(readonly_already_loaded, Default::default());

    // Ensure accounts
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![readonly_already_loaded],
                writable: vec![],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Check proper behaviour
    assert!(account_dumper.was_untouched(&readonly_already_loaded));
    assert_eq!(manager.last_commit(&readonly_already_loaded), None);

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_readonly_account_cloned_but_not_in_our_validator() {
    init_logger!();
    let readonly_undelegated = Pubkey::new_unique();

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
    account_fetcher.set_pda_account(readonly_undelegated, 42);
    assert!(manager
        .account_cloner
        .clone_account(&readonly_undelegated)
        .await
        .is_ok());
    assert!(account_dumper.was_dumped_as_pda_account(&readonly_undelegated));
    account_dumper.clear_history();

    // Ensure accounts
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![readonly_undelegated],
                writable: vec![],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Check proper behaviour
    assert!(account_dumper.was_untouched(&readonly_undelegated));
    assert!(manager.last_commit(&readonly_undelegated).is_none());

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_readonly_account_cloned_but_has_been_updated_on_chain() {
    init_logger!();
    let readonly_undelegated = Pubkey::new_unique();

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
    account_fetcher.set_pda_account(readonly_undelegated, 42);
    assert!(manager
        .account_cloner
        .clone_account(&readonly_undelegated)
        .await
        .is_ok());
    assert!(account_dumper.was_dumped_as_pda_account(&readonly_undelegated));
    account_dumper.clear_history();

    // Make the account re-fetchable at a later slot with a pending update
    account_updates.set_known_update_slot(readonly_undelegated, 55);
    account_fetcher.set_pda_account(readonly_undelegated, 55);

    // Ensure accounts
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![readonly_undelegated],
                writable: vec![],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Check proper behaviour
    assert!(account_dumper.was_dumped_as_pda_account(&readonly_undelegated));
    assert!(manager.last_commit(&readonly_undelegated).is_none());

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_readonly_account_cloned_and_no_recent_update_on_chain() {
    init_logger!();
    let readonly_undelegated = Pubkey::new_unique();

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
    account_fetcher.set_pda_account(readonly_undelegated, 11);
    assert!(manager
        .account_cloner
        .clone_account(&readonly_undelegated)
        .await
        .is_ok());
    assert!(account_dumper.was_dumped_as_pda_account(&readonly_undelegated));
    account_dumper.clear_history();

    // Account was updated, but before the last clone's slot
    account_updates.set_known_update_slot(readonly_undelegated, 5);

    // Ensure accounts
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![readonly_undelegated],
                writable: vec![],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Check proper behaviour
    assert!(account_dumper.was_untouched(&readonly_undelegated));
    assert!(manager.last_commit(&readonly_undelegated).is_none());

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_readonly_account_in_our_validator_and_unseen_writable() {
    init_logger!();
    let readonly_already_loaded = Pubkey::new_unique();
    let writable_delegated = Pubkey::new_unique();

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
    internal_account_provider.set(readonly_already_loaded, Default::default());
    account_fetcher.set_delegated_account(writable_delegated, 42, 11);

    // Ensure accounts
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![readonly_already_loaded],
                writable: vec![writable_delegated],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Check proper behaviour
    assert!(account_dumper.was_untouched(&readonly_already_loaded));
    assert!(manager.last_commit(&readonly_already_loaded).is_none());

    assert!(account_dumper.was_dumped_as_delegated_account(&writable_delegated));
    assert!(manager.last_commit(&writable_delegated).is_some());

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_delegated_with_owner_and_undelegated_writable_payer() {
    init_logger!();
    let writable_undelegated_payer = Pubkey::new_unique();
    let writable_delegated = Pubkey::new_unique();

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

    // One delegated, one undelegated system account
    account_fetcher.set_system_account(writable_undelegated_payer, 42);
    account_fetcher.set_delegated_account(writable_delegated, 42, 11);

    // Ensure accounts
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![],
                writable: vec![writable_undelegated_payer, writable_delegated],
                payer: writable_undelegated_payer,
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Check proper behaviour
    assert!(account_dumper
        .was_dumped_as_system_account(&writable_undelegated_payer));
    assert!(manager.last_commit(&writable_undelegated_payer).is_none());

    assert!(account_dumper.was_dumped_as_delegated_account(&writable_delegated));
    assert!(manager.last_commit(&writable_delegated).is_some());

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_one_delegated_and_one_new_account_writable() {
    init_logger!();
    let writable_delegated = Pubkey::new_unique();
    let writable_new_account = Pubkey::new_unique();

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

    // One writable delegated and one new account
    account_fetcher.set_delegated_account(writable_delegated, 42, 11);
    account_fetcher.set_new_account(writable_new_account, 42);

    // Ensure account
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![],
                writable: vec![writable_new_account, writable_delegated],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Check proper behaviour
    assert!(account_dumper.was_dumped_as_delegated_account(&writable_delegated));
    assert!(manager.last_commit(&writable_delegated).is_some());

    assert!(account_dumper.was_dumped_as_new_account(&writable_new_account));
    assert!(manager.last_commit(&writable_new_account).is_none());

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_multiple_accounts_coming_in_over_time() {
    init_logger!();
    let readonly1_undelegated = Pubkey::new_unique();
    let readonly2_undelegated = Pubkey::new_unique();
    let readonly3_undelegated = Pubkey::new_unique();
    let writable1_delegated = Pubkey::new_unique();
    let writable2_delegated = Pubkey::new_unique();

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
    account_fetcher.set_pda_account(readonly1_undelegated, 42);
    account_fetcher.set_pda_account(readonly2_undelegated, 42);
    account_fetcher.set_pda_account(readonly3_undelegated, 42);
    account_fetcher.set_delegated_account(writable1_delegated, 42, 11);
    account_fetcher.set_delegated_account(writable2_delegated, 42, 11);

    // First Transaction
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![
                        readonly1_undelegated,
                        readonly2_undelegated,
                    ],
                    writable: vec![writable1_delegated],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(
            account_dumper.was_dumped_as_pda_account(&readonly1_undelegated)
        );
        assert!(manager.last_commit(&readonly1_undelegated).is_none());

        assert!(
            account_dumper.was_dumped_as_pda_account(&readonly2_undelegated)
        );
        assert!(manager.last_commit(&readonly2_undelegated).is_none());

        assert!(account_dumper.was_untouched(&readonly3_undelegated));
        assert!(manager.last_commit(&readonly3_undelegated).is_none());

        assert!(account_dumper
            .was_dumped_as_delegated_account(&writable1_delegated));
        assert!(manager.last_commit(&writable1_delegated).is_some());

        assert!(account_dumper.was_untouched(&writable2_delegated));
        assert!(manager.last_commit(&writable2_delegated).is_none());
    }

    account_dumper.clear_history();

    // Second Transaction
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![
                        readonly1_undelegated,
                        readonly2_undelegated,
                    ],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper.was_untouched(&readonly1_undelegated));
        assert!(manager.last_commit(&readonly1_undelegated).is_none());

        assert!(account_dumper.was_untouched(&readonly2_undelegated));
        assert!(manager.last_commit(&readonly2_undelegated).is_none());

        assert!(account_dumper.was_untouched(&readonly3_undelegated));
        assert!(manager.last_commit(&readonly3_undelegated).is_none());

        assert!(account_dumper.was_untouched(&writable1_delegated));
        assert!(manager.last_commit(&writable1_delegated).is_some());

        assert!(account_dumper.was_untouched(&writable2_delegated));
        assert!(manager.last_commit(&writable2_delegated).is_none());
    }

    account_dumper.clear_history();

    // Third Transaction
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![
                        readonly2_undelegated,
                        readonly3_undelegated,
                    ],
                    writable: vec![writable2_delegated],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper.was_untouched(&readonly1_undelegated));
        assert!(manager.last_commit(&readonly1_undelegated).is_none());

        assert!(account_dumper.was_untouched(&readonly2_undelegated));
        assert!(manager.last_commit(&readonly2_undelegated).is_none());

        assert!(
            account_dumper.was_dumped_as_pda_account(&readonly3_undelegated)
        );
        assert!(manager.last_commit(&readonly3_undelegated).is_none());

        assert!(account_dumper.was_untouched(&writable1_delegated));
        assert!(manager.last_commit(&writable1_delegated).is_some());

        assert!(account_dumper
            .was_dumped_as_delegated_account(&writable2_delegated));
        assert!(manager.last_commit(&writable2_delegated).is_some());
    }

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_writable_account_fails_to_validate() {
    init_logger!();
    let writable_new_account = Pubkey::new_unique();

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

    // One new account
    account_fetcher.set_new_account(writable_new_account, 42);

    // Ensure accounts
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![],
                writable: vec![writable_new_account],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;

    // Check proper behaviour
    assert!(matches!(
        result,
        Err(AccountsError::TranswiseError(
            TranswiseError::WritablesIncludeNewAccounts { .. }
        ))
    ));

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_accounts_seen_first_as_readonly_can_be_used_as_writable_later(
) {
    init_logger!();
    let account_delegated = Pubkey::new_unique();

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
    account_fetcher.set_delegated_account(account_delegated, 42, 11);

    // First Transaction uses the account as a readable (it should still be detected as a delegated)
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![account_delegated],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(
            account_dumper.was_dumped_as_delegated_account(&account_delegated)
        );
        assert!(manager.last_commit(&account_delegated).is_some());
    }

    account_dumper.clear_history();

    // Second Transaction uses the same account as a writable, nothing should happen
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![],
                    writable: vec![account_delegated],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper.was_untouched(&account_delegated));
        assert!(manager.last_commit(&account_delegated).is_some());
    }

    account_dumper.clear_history();

    // Third transaction reuse the account as readable, nothing should happen then
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![account_delegated],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper.was_untouched(&account_delegated));
        assert!(manager.last_commit(&account_delegated).is_some());
    }

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_accounts_already_known_can_be_reused_as_writable_later() {
    init_logger!();
    let account_delegated = Pubkey::new_unique();

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
    internal_account_provider.set(account_delegated, Default::default());
    account_fetcher.set_delegated_account(account_delegated, 42, 11);

    // First Transaction should not clone the account to use it as readonly
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![account_delegated],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper.was_untouched(&account_delegated));
        assert!(manager.last_commit(&account_delegated).is_none());
    }

    account_dumper.clear_history();

    // Second Transaction trying to use it as a writable should fail because of a local override
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![],
                    writable: vec![account_delegated],
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
    let account_undelegated = Pubkey::new_unique();

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
    account_fetcher.set_pda_account(account_undelegated, 42);
    assert!(manager
        .account_cloner
        .clone_account(&account_undelegated)
        .await
        .is_ok());
    assert!(account_dumper.was_dumped_as_pda_account(&account_undelegated));
    account_dumper.clear_history();

    // We detect an update that's more recent
    account_updates.set_known_update_slot(account_undelegated, 88);

    // But for this case, the account fetcher is too slow and can only fetch an old version for some reason
    account_fetcher.set_pda_account(account_undelegated, 77);

    // The first transaction should need to clone since there was an update
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![account_undelegated],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper.was_dumped_as_pda_account(&account_undelegated));
        assert!(manager.last_commit(&account_undelegated).is_none());
    }

    account_dumper.clear_history();

    // The second transaction should also need to clone because the previous version we cloned was too old
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![account_undelegated],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper.was_dumped_as_pda_account(&account_undelegated));
        assert!(manager.last_commit(&account_undelegated).is_none());
    }

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_accounts_already_cloned_can_be_reused_without_updates() {
    init_logger!();
    let readonly_undelegated = Pubkey::new_unique();

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
    account_fetcher.set_pda_account(readonly_undelegated, 42);
    assert!(manager
        .account_cloner
        .clone_account(&readonly_undelegated)
        .await
        .is_ok());
    assert!(account_dumper.was_dumped_as_pda_account(&readonly_undelegated));
    account_dumper.clear_history();

    // The account has been updated on-chain since the last clone
    account_fetcher.set_pda_account(readonly_undelegated, 66);
    account_updates.set_known_update_slot(readonly_undelegated, 66);

    // The first transaction should need to clone since the account was updated on-chain since the last clone
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![readonly_undelegated],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper.was_dumped_as_pda_account(&readonly_undelegated));
        assert!(manager.last_commit(&readonly_undelegated).is_none());
    }

    account_dumper.clear_history();

    // The second transaction should not need to clone since the account was not updated since the first transaction's clone
    {
        // Ensure accounts
        let result = manager
            .ensure_accounts_from_holder(
                TransactionAccountsHolder {
                    readonly: vec![readonly_undelegated],
                    writable: vec![],
                    payer: Pubkey::new_unique(),
                },
                "tx-sig".to_string(),
            )
            .await;
        assert!(result.is_ok());

        // Check proper behaviour
        assert!(account_dumper.was_untouched(&readonly_undelegated));
        assert!(manager.last_commit(&readonly_undelegated).is_none());
    }

    // Cleanup
    cancel.cancel();
    assert!(handle.await.is_ok());
}
