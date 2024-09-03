use std::{collections::HashSet, sync::Arc};

use conjunto_transwise::{
    errors::TranswiseError,
    transaction_accounts_extractor::TransactionAccountsExtractorImpl,
    transaction_accounts_holder::TransactionAccountsHolder,
    transaction_accounts_validator::TransactionAccountsValidatorImpl,
};
use sleipnir_account_cloner::{
    RemoteAccountClonerClient, RemoteAccountClonerWorker,
};
use sleipnir_account_dumper::AccountDumperStub;
use sleipnir_account_fetcher::AccountFetcherStub;
use sleipnir_account_updates::AccountUpdatesStub;
use sleipnir_accounts::{
    errors::AccountsError, ExternalAccountsManager, LifecycleMode,
};
use sleipnir_accounts_api::InternalAccountProviderStub;
use solana_sdk::{native_token::LAMPORTS_PER_SOL, pubkey::Pubkey};
use stubs::{
    account_committer_stub::AccountCommitterStub,
    scheduled_commits_processor_stub::ScheduledCommitsProcessorStub,
    StubbedAccountsManager,
};
use test_tools_core::init_logger;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

mod stubs;

#[allow(clippy::too_many_arguments)]
fn setup_with_lifecycle(
    internal_account_provider: InternalAccountProviderStub,
    account_fetcher: AccountFetcherStub,
    account_updates: AccountUpdatesStub,
    account_dumper: AccountDumperStub,
    lifecycle: LifecycleMode,
) -> (StubbedAccountsManager, CancellationToken, JoinHandle<()>) {
    let validator_auth_id = Pubkey::new_unique();
    let cancellation_token = CancellationToken::new();

    let remote_account_cloner_worker = RemoteAccountClonerWorker::new(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
        HashSet::new(),
        Some(1_000_000_000),
        lifecycle.allow_cloning_undelegated_non_programs(),
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

    let fetchable_at_slot = 42;
    account_fetcher.add_undelegated(readonly_undelegated, fetchable_at_slot);

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly_undelegated],
        writable: vec![],
        payer: Pubkey::new_unique(),
    };

    manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;

    assert!(account_dumper
        .get_dumped_pda_accounts()
        .contains(&readonly_undelegated));

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

    internal_account_provider.add(readonly_already_loaded, Default::default());

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly_already_loaded],
        writable: vec![],
        payer: Pubkey::new_unique(),
    };

    manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;

    assert!(!account_dumper
        .get_dumped_pda_accounts()
        .contains(&readonly_already_loaded));

    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_readonly_account_cloned_but_not_in_our_validator() {
    init_logger!();
    let readonly_already_cloned = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
    );

    let cloned_at_slot = 42;
    manager
        .external_readonly_accounts
        .insert(readonly_already_cloned, cloned_at_slot);

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly_already_cloned],
        writable: vec![],
        payer: Pubkey::new_unique(),
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;

    assert_eq!(result.unwrap().len(), 0);

    assert!(!account_dumper
        .get_dumped_pda_accounts()
        .contains(&readonly_already_cloned));

    cancel.cancel();
    assert!(handle.await.is_ok());
}

#[tokio::test]
async fn test_ensure_readonly_account_tracked_but_has_been_updated_on_chain() {
    init_logger!();
    let readonly_undelegated = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_dumper = AccountDumperStub::default();
    let account_updates = AccountUpdatesStub::default();

    let cloned_at_slot = 11;
    let updated_last_at_slot = 42;
    let fetchable_at_slot = 55;

    account_fetcher.add_undelegated(readonly_undelegated, fetchable_at_slot);
    account_updates
        .add_known_update_slot(readonly_undelegated, updated_last_at_slot);

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
    );

    manager
        .external_readonly_accounts
        .insert(readonly_undelegated, cloned_at_slot);

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly_undelegated],
        writable: vec![],
        payer: Pubkey::new_unique(),
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;

    assert_eq!(result.unwrap().len(), 1);

    assert!(account_dumper
        .get_dumped_pda_accounts()
        .contains(&readonly_undelegated));

    assert!(manager
        .external_readonly_accounts
        .has(&readonly_undelegated));
    assert!(manager.external_writable_accounts.is_empty());
}

#[tokio::test]
async fn test_ensure_readonly_account_tracked_and_no_recent_update_on_chain() {
    init_logger!();
    let readonly_undelegated = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_dumper = AccountDumperStub::default();
    let account_updates = AccountUpdatesStub::default();

    let cloned_at_slot = 42;
    let updated_last_at_slot = 11;

    account_updates
        .add_known_update_slot(readonly_undelegated, updated_last_at_slot);

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
    );

    manager
        .external_readonly_accounts
        .insert(readonly_undelegated, cloned_at_slot);

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly_undelegated],
        writable: vec![],
        payer: Pubkey::new_unique(),
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;

    assert_eq!(result.unwrap().len(), 0);

    assert!(!account_dumper
        .get_dumped_pda_accounts()
        .contains(&readonly_undelegated));

    assert!(manager
        .external_readonly_accounts
        .has(&readonly_undelegated));
    assert!(manager.external_writable_accounts.is_empty());
}

#[tokio::test]
async fn test_ensure_readonly_account_in_our_validator_and_unseen_writable() {
    init_logger!();
    let readonly_already_loaded = Pubkey::new_unique();
    let writable_delegated = Pubkey::new_unique();
    let writable_delegated_owner = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_dumper = AccountDumperStub::default();
    let account_updates = AccountUpdatesStub::default();

    internal_account_provider.add(readonly_already_loaded, Default::default());

    let fetchable_at_slot = 42;
    account_fetcher.add_delegated(
        writable_delegated,
        writable_delegated_owner,
        fetchable_at_slot,
    );

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly_already_loaded],
        writable: vec![writable_delegated],
        payer: Pubkey::new_unique(),
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;
    assert_eq!(result.unwrap().len(), 1);

    assert!(!account_dumper
        .get_dumped_pda_accounts()
        .contains(&readonly_already_loaded));
    assert!(account_dumper
        .get_dumped_pda_accounts()
        .contains(&writable_delegated));

    assert!(manager
        .account_cloner
        .did_not_override_lamports(&writable_delegated));
    assert!(manager
        .account_cloner
        .did_override_owner(&writable_delegated, &writable_delegated_owner));

    assert!(manager.external_readonly_accounts.is_empty());
    assert!(manager.external_writable_accounts.has(&writable_delegated));
}

#[tokio::test]
async fn test_ensure_delegated_with_owner_and_unlocked_writable_payer() {
    init_logger!();
    let writable_delegated = Pubkey::new_unique();
    let writable_delegated_owner = Pubkey::new_unique();
    let writable_undelegated_payer = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_dumper = AccountDumperStub::default();
    let account_updates = AccountUpdatesStub::default();

    let fetchable_at_slot = 45;
    account_fetcher.add_delegated(
        writable_delegated,
        writable_delegated_owner,
        fetchable_at_slot,
    );
    account_fetcher
        .add_undelegated(writable_undelegated_payer, fetchable_at_slot);

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![],
        writable: vec![writable_undelegated_payer, writable_delegated],
        payer: writable_undelegated_payer,
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;
    assert_eq!(result.unwrap().len(), 2);

    assert!(account_dumper
        .get_dumped_pda_accounts()
        .contains(&writable_delegated));
    assert!(manager
        .account_cloner
        .did_clone(&writable_undelegated_payer));

    assert!(manager
        .account_cloner
        .did_override_owner(&writable_delegated, &writable_delegated_owner));
    assert!(manager
        .account_cloner
        .did_not_override_lamports(&writable_delegated));

    assert!(manager.account_cloner.did_override_lamports(
        &writable_undelegated_payer,
        LAMPORTS_PER_SOL * 1_000
    ));
    assert!(manager
        .account_cloner
        .did_not_override_owner(&writable_undelegated_payer));

    assert!(manager.external_readonly_accounts.is_empty());
    assert!(manager
        .external_writable_accounts
        .has(&writable_undelegated_payer));
    assert!(manager.external_writable_accounts.has(&writable_delegated));
}

#[tokio::test]
async fn test_ensure_one_delegated_and_one_new_account_writable() {
    init_logger!();
    let writable_delegated = Pubkey::new_unique();
    let writable_new_account = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_dumper = AccountDumperStub::default();
    let account_updates = AccountUpdatesStub::default();

    let fetchable_at_slot = 42;
    account_fetcher.add_delegated(
        writable_delegated,
        Pubkey::new_unique(),
        fetchable_at_slot,
    );

    // Note: since we use a writable new account, we need to allow it as part of the configuration
    // We can't use an ephemeral's configuration, that forbids new accounts to be writable
    let manager = setup_with_lifecycle(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
        LifecycleMode::Replica,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![],
        writable: vec![writable_new_account, writable_delegated],
        payer: Pubkey::new_unique(),
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;

    assert_eq!(result.unwrap().len(), 1);

    assert!(account_dumper
        .get_dumped_pda_accounts()
        .contains(&writable_delegated));
    assert!(!account_dumper
        .get_dumped_pda_accounts()
        .contains(&writable_new_account));

    assert!(manager.external_readonly_accounts.is_empty());

    assert_eq!(manager.external_writable_accounts.len(), 1);
    assert!(manager.external_writable_accounts.has(&writable_delegated));
    assert!(!manager
        .external_writable_accounts
        .has(&writable_new_account));
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
    let account_dumper = AccountDumperStub::default();
    let account_updates = AccountUpdatesStub::default();

    let fetchable_at_slot = 42;
    account_fetcher.add_undelegated(readonly1_undelegated, fetchable_at_slot);
    account_fetcher.add_undelegated(readonly2_undelegated, fetchable_at_slot);
    account_fetcher.add_undelegated(readonly3_undelegated, fetchable_at_slot);
    account_fetcher.add_delegated(
        writable1_delegated,
        Pubkey::new_unique(),
        fetchable_at_slot,
    );
    account_fetcher.add_delegated(
        writable2_delegated,
        Pubkey::new_unique(),
        fetchable_at_slot,
    );

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
    );

    // First Transaction
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![readonly1_undelegated, readonly2_undelegated],
            writable: vec![writable1_delegated],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;
        assert_eq!(result.unwrap().len(), 3);

        assert!(account_dumper
            .get_dumped_pda_accounts()
            .contains(&readonly1_undelegated));
        assert!(account_dumper
            .get_dumped_pda_accounts()
            .contains(&readonly2_undelegated));
        assert!(!account_dumper
            .get_dumped_pda_accounts()
            .contains(&readonly3_undelegated));

        assert!(account_dumper
            .get_dumped_pda_accounts()
            .contains(&writable1_delegated));
        assert!(!account_dumper
            .get_dumped_pda_accounts()
            .contains(&writable2_delegated));

        assert!(manager
            .external_readonly_accounts
            .has(&readonly1_undelegated));
        assert!(manager
            .external_readonly_accounts
            .has(&readonly2_undelegated));
        assert!(!manager
            .external_readonly_accounts
            .has(&readonly3_undelegated));

        assert!(manager.external_writable_accounts.has(&writable1_delegated));
        assert!(!manager.external_writable_accounts.has(&writable2_delegated));
    }

    manager.account_cloner.clear();

    // Second Transaction
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![readonly1_undelegated, readonly2_undelegated],
            writable: vec![],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;
        assert!(result.unwrap().is_empty());

        assert!(!account_dumper
            .get_dumped_pda_accounts()
            .contains(&readonly1_undelegated));
        assert!(!account_dumper
            .get_dumped_pda_accounts()
            .contains(&readonly2_undelegated));
        assert!(!account_dumper
            .get_dumped_pda_accounts()
            .contains(&readonly3_undelegated));

        assert!(!account_dumper
            .get_dumped_pda_accounts()
            .contains(&writable1_delegated));
        assert!(!account_dumper
            .get_dumped_pda_accounts()
            .contains(&writable2_delegated));

        assert!(manager
            .external_readonly_accounts
            .has(&readonly1_undelegated));
        assert!(manager
            .external_readonly_accounts
            .has(&readonly2_undelegated));
        assert!(!manager
            .external_readonly_accounts
            .has(&readonly3_undelegated));

        assert!(manager.external_writable_accounts.has(&writable1_delegated));
        assert!(!manager.external_writable_accounts.has(&writable2_delegated));
    }

    manager.account_cloner.clear();

    // Third Transaction
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![readonly2_undelegated, readonly3_undelegated],
            writable: vec![writable2_delegated],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;
        assert_eq!(result.unwrap().len(), 2);

        assert!(!account_dumper
            .get_dumped_pda_accounts()
            .contains(&readonly1_undelegated));
        assert!(!account_dumper
            .get_dumped_pda_accounts()
            .contains(&readonly2_undelegated));
        assert!(account_dumper
            .get_dumped_pda_accounts()
            .contains(&readonly3_undelegated));

        assert!(!account_dumper
            .get_dumped_pda_accounts()
            .contains(&writable1_delegated));
        assert!(account_dumper
            .get_dumped_pda_accounts()
            .contains(&writable2_delegated));

        assert!(manager
            .external_readonly_accounts
            .has(&readonly1_undelegated));
        assert!(manager
            .external_readonly_accounts
            .has(&readonly2_undelegated));
        assert!(manager
            .external_readonly_accounts
            .has(&readonly3_undelegated));

        assert!(manager.external_writable_accounts.has(&writable1_delegated));
        assert!(manager.external_writable_accounts.has(&writable2_delegated));
    }
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
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![],
        writable: vec![writable_new_account],
        payer: Pubkey::new_unique(),
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;

    assert!(matches!(
        result,
        Err(AccountsError::TranswiseError(
            TranswiseError::WritablesIncludeNewAccounts { .. }
        ))
    ));
}

#[tokio::test]
async fn test_ensure_accounts_seen_first_as_readonly_can_be_used_as_writable_later(
) {
    init_logger!();
    let account_delegated = Pubkey::new_unique();
    let account_delegated_owner = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_dumper = AccountDumperStub::default();
    let account_updates = AccountUpdatesStub::default();

    let fetchable_at_slot = 42;
    account_fetcher.add_delegated(
        account_delegated,
        account_delegated_owner,
        fetchable_at_slot,
    );

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
    );

    // First Transaction uses the account as a readable
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![account_delegated],
            writable: vec![],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;

        assert_eq!(result.unwrap().len(), 1);

        assert!(account_dumper
            .get_dumped_pda_accounts()
            .contains(&account_delegated));

        assert!(manager
            .account_cloner
            .did_not_override_owner(&account_delegated));

        assert!(manager.external_readonly_accounts.has(&account_delegated));
        assert!(manager.external_writable_accounts.is_empty());
    }

    manager.account_cloner.clear();

    // Second Transaction uses the same account as a writable
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![],
            writable: vec![account_delegated],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;

        assert_eq!(result.unwrap().len(), 1);

        // TODO(vbrunet) - this should not need to re-clone
        assert!(account_dumper
            .get_dumped_pda_accounts()
            .contains(&account_delegated));

        assert!(manager
            .account_cloner
            .did_override_owner(&account_delegated, &account_delegated_owner));

        assert!(manager.external_readonly_accounts.is_empty());
        assert!(manager.external_writable_accounts.has(&account_delegated));
    }

    manager.account_cloner.clear();

    // Third transaction reuse the account as readable, nothing should happen then
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![account_delegated],
            writable: vec![],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;

        assert_eq!(result.unwrap().len(), 0);

        assert!(!account_dumper
            .get_dumped_pda_accounts()
            .contains(&account_delegated));

        assert!(manager.external_readonly_accounts.is_empty());
        assert!(manager.external_writable_accounts.has(&account_delegated));
    }
}

#[tokio::test]
async fn test_ensure_accounts_already_known_can_be_reused_as_writable_later() {
    init_logger!();
    let account_delegated = Pubkey::new_unique();
    let account_delegated_owner = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_dumper = AccountDumperStub::default();
    let account_updates = AccountUpdatesStub::default();

    internal_account_provider.add(account_delegated, Default::default());

    let fetchable_at_slot = 42;
    account_fetcher.add_delegated(
        account_delegated,
        account_delegated_owner,
        fetchable_at_slot,
    );

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
    );

    // First Transaction does not need to re-clone account to use it as readonly
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![account_delegated],
            writable: vec![],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;

        assert_eq!(result.unwrap().len(), 0);

        assert!(!account_dumper
            .get_dumped_pda_accounts()
            .contains(&account_delegated));

        assert!(manager
            .account_cloner
            .did_not_override_owner(&account_delegated));

        assert!(manager.external_readonly_accounts.is_empty());
        assert!(manager.external_writable_accounts.is_empty());
    }

    manager.account_cloner.clear();

    // Second Transaction does need to re-clone account to override it, so it can be used as a writable
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![],
            writable: vec![account_delegated],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;

        assert_eq!(result.unwrap().len(), 1);

        assert!(account_dumper
            .get_dumped_pda_accounts()
            .contains(&account_delegated));

        assert!(manager
            .account_cloner
            .did_override_owner(&account_delegated, &account_delegated_owner));

        assert!(manager.external_readonly_accounts.is_empty());
        assert!(manager.external_writable_accounts.has(&account_delegated));
    }
}

#[tokio::test]
async fn test_ensure_accounts_already_cloned_needs_reclone_after_updates() {
    init_logger!();
    let account_undelegated = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_dumper = AccountDumperStub::default();
    let account_updates = AccountUpdatesStub::default();

    let cloned_at_slot = 11;
    let fetchable_at_slot = 14;
    let last_updated_at_slot = 42;

    internal_account_provider.add(account_undelegated, Default::default());
    account_fetcher.add_undelegated(account_undelegated, fetchable_at_slot);
    account_updates
        .add_known_update_slot(account_undelegated, last_updated_at_slot);

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
    );

    manager
        .external_readonly_accounts
        .insert(account_undelegated, cloned_at_slot);

    // The first transaction should need to clone since the cloned_at_slot is before last_updated_at_slot
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![account_undelegated],
            writable: vec![],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;

        assert_eq!(result.unwrap().len(), 1);

        assert!(account_dumper
            .get_dumped_pda_accounts()
            .contains(&account_undelegated));

        assert!(manager.external_readonly_accounts.has(&account_undelegated));
        assert!(manager.external_writable_accounts.is_empty());
    }

    manager.account_cloner.clear();

    // The second transaction should also need to clone because the fetchable_at_slot is before last_updated_at_slot
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![account_undelegated],
            writable: vec![],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;

        assert_eq!(result.unwrap().len(), 1);

        assert!(account_dumper
            .get_dumped_pda_accounts()
            .contains(&account_undelegated));

        assert!(manager.external_readonly_accounts.has(&account_undelegated));
        assert!(manager.external_writable_accounts.is_empty());
    }
}

#[tokio::test]
async fn test_ensure_accounts_already_known_can_be_reused_without_updates() {
    init_logger!();
    let account_undelegated = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_dumper = AccountDumperStub::default();
    let account_updates = AccountUpdatesStub::default();

    let cloned_at_slot = 11;
    let last_updated_at_slot = 15;
    let fetchable_at_slot = 20;

    internal_account_provider.add(account_undelegated, Default::default());
    account_fetcher.add_undelegated(account_undelegated, fetchable_at_slot);
    account_updates
        .add_known_update_slot(account_undelegated, last_updated_at_slot);

    let (manager, cancel, handle) = setup_ephem(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
    );

    manager
        .external_readonly_accounts
        .insert(account_undelegated, cloned_at_slot);

    // The first transaction should need to clone since the account was updated on-chain since the cloned_at_slot
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![account_undelegated],
            writable: vec![],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;

        assert_eq!(result.unwrap().len(), 1);

        assert!(account_dumper
            .get_dumped_pda_accounts()
            .contains(&account_undelegated));

        assert!(manager.external_readonly_accounts.has(&account_undelegated));
        assert!(manager.external_writable_accounts.is_empty());
    }

    manager.account_cloner.clear();

    // The second transaction should not need to clone since the account was not updated since the first transaction's clone
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![account_undelegated],
            writable: vec![],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;

        assert_eq!(result.unwrap().len(), 0);

        assert!(!account_dumper
            .get_dumped_pda_accounts()
            .contains(&account_undelegated));

        assert!(manager.external_readonly_accounts.has(&account_undelegated));
        assert!(manager.external_writable_accounts.is_empty());
    }
}
