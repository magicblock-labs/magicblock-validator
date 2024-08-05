use std::sync::Arc;

use conjunto_transwise::{
    errors::TranswiseError,
    transaction_accounts_extractor::TransactionAccountsExtractorImpl,
    transaction_accounts_holder::TransactionAccountsHolder,
    transaction_accounts_validator::TransactionAccountsValidatorImpl,
};
use sleipnir_accounts::{
    errors::AccountsError, ExternalAccountsManager, ExternalReadonlyMode,
    ExternalWritableMode,
};
use solana_sdk::{native_token::LAMPORTS_PER_SOL, pubkey::Pubkey};
use stubs::{
    account_cloner_stub::AccountClonerStub,
    account_committer_stub::AccountCommitterStub,
    account_fetcher_stub::AccountFetcherStub,
    account_updates_stub::AccountUpdatesStub,
    internal_account_provider_stub::InternalAccountProviderStub,
    scheduled_commits_processor_stub::ScheduledCommitsProcessorStub,
};
use test_tools_core::init_logger;

mod stubs;

fn setup_customized(
    internal_account_provider: InternalAccountProviderStub,
    account_fetcher: AccountFetcherStub,
    account_cloner: AccountClonerStub,
    account_committer: AccountCommitterStub,
    account_updates: AccountUpdatesStub,
    external_readonly_mode: ExternalReadonlyMode,
    external_writable_mode: ExternalWritableMode,
    create_accounts: bool,
) -> ExternalAccountsManager<
    InternalAccountProviderStub,
    AccountFetcherStub,
    AccountClonerStub,
    AccountCommitterStub,
    AccountUpdatesStub,
    TransactionAccountsExtractorImpl,
    TransactionAccountsValidatorImpl,
    ScheduledCommitsProcessorStub,
> {
    let validator_auth_id = Pubkey::new_unique();
    ExternalAccountsManager {
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer: Arc::new(account_committer),
        account_updates,
        transaction_accounts_extractor: TransactionAccountsExtractorImpl,
        transaction_accounts_validator: TransactionAccountsValidatorImpl,
        external_readonly_accounts: Default::default(),
        external_writable_accounts: Default::default(),
        scheduled_commits_processor: ScheduledCommitsProcessorStub::default(),
        external_readonly_mode,
        external_writable_mode,
        create_accounts,
        payer_init_lamports: Some(1_000 * LAMPORTS_PER_SOL),
        validator_id: validator_auth_id,
    }
}

fn setup_standard(
    internal_account_provider: InternalAccountProviderStub,
    account_fetcher: AccountFetcherStub,
    account_cloner: AccountClonerStub,
    account_committer: AccountCommitterStub,
    account_updates: AccountUpdatesStub,
) -> ExternalAccountsManager<
    InternalAccountProviderStub,
    AccountFetcherStub,
    AccountClonerStub,
    AccountCommitterStub,
    AccountUpdatesStub,
    TransactionAccountsExtractorImpl,
    TransactionAccountsValidatorImpl,
    ScheduledCommitsProcessorStub,
> {
    setup_customized(
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer,
        account_updates,
        ExternalReadonlyMode::All,
        ExternalWritableMode::Delegated,
        false,
    )
}

#[tokio::test]
async fn test_ensure_readonly_account_not_tracked_nor_in_our_validator() {
    init_logger!();
    let readonly_undelegated = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let mut account_fetcher = AccountFetcherStub::default();
    let account_cloner = AccountClonerStub::default();
    let account_committer = AccountCommitterStub::default();
    let account_updates = AccountUpdatesStub::default();

    let fetchable_at_slot = 42;
    account_fetcher.add_undelegated(readonly_undelegated, fetchable_at_slot);

    let manager = setup_standard(
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer,
        account_updates,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly_undelegated],
        writable: vec![],
        payer: Pubkey::new_unique(),
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;

    assert_eq!(result.unwrap().len(), 1);

    assert!(manager.account_cloner.did_clone(&readonly_undelegated));

    assert!(manager
        .external_readonly_accounts
        .has(&readonly_undelegated));
    assert!(manager.external_writable_accounts.is_empty());
}

#[tokio::test]
async fn test_ensure_readonly_account_not_tracked_but_in_our_validator() {
    init_logger!();
    let readonly_already_loaded = Pubkey::new_unique();

    let mut internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_cloner = AccountClonerStub::default();
    let account_committer = AccountCommitterStub::default();
    let account_updates = AccountUpdatesStub::default();

    internal_account_provider.add(readonly_already_loaded, Default::default());

    let manager = setup_standard(
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer,
        account_updates,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly_already_loaded],
        writable: vec![],
        payer: Pubkey::new_unique(),
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;

    assert_eq!(result.unwrap().len(), 0);

    assert!(!manager.account_cloner.did_clone(&readonly_already_loaded));

    assert!(manager.external_readonly_accounts.is_empty());
    assert!(manager.external_writable_accounts.is_empty());
}

#[tokio::test]
async fn test_ensure_readonly_account_cloned_but_not_in_our_validator() {
    init_logger!();
    let readonly_already_cloned = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_cloner = AccountClonerStub::default();
    let account_committer = AccountCommitterStub::default();
    let account_updates = AccountUpdatesStub::default();

    let manager = setup_standard(
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer,
        account_updates,
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

    assert!(!manager.account_cloner.did_clone(&readonly_already_cloned));

    assert!(manager
        .external_readonly_accounts
        .has(&readonly_already_cloned));
    assert!(manager.external_writable_accounts.is_empty());
}

#[tokio::test]
async fn test_ensure_readonly_account_tracked_but_has_been_updated_on_chain() {
    init_logger!();
    let readonly_undelegated = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let mut account_fetcher = AccountFetcherStub::default();
    let account_cloner = AccountClonerStub::default();
    let account_committer = AccountCommitterStub::default();
    let mut account_updates = AccountUpdatesStub::default();

    let cloned_at_slot = 11;
    let updated_last_at_slot = 42;
    let fetchable_at_slot = 55;

    account_fetcher.add_undelegated(readonly_undelegated, fetchable_at_slot);
    account_updates
        .add_known_update(readonly_undelegated, updated_last_at_slot);

    let manager = setup_standard(
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer,
        account_updates,
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

    assert!(manager.account_cloner.did_clone(&readonly_undelegated));

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
    let account_cloner = AccountClonerStub::default();
    let account_committer = AccountCommitterStub::default();
    let mut account_updates = AccountUpdatesStub::default();

    let cloned_at_slot = 42;
    let updated_last_at_slot = 11;

    account_updates
        .add_known_update(readonly_undelegated, updated_last_at_slot);

    let manager = setup_standard(
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer,
        account_updates,
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

    assert!(!manager.account_cloner.did_clone(&readonly_undelegated));

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

    let mut internal_account_provider = InternalAccountProviderStub::default();
    let mut account_fetcher = AccountFetcherStub::default();
    let account_cloner = AccountClonerStub::default();
    let account_committer = AccountCommitterStub::default();
    let account_updates = AccountUpdatesStub::default();

    internal_account_provider.add(readonly_already_loaded, Default::default());

    let fetchable_at_slot = 42;
    account_fetcher.add_delegated(
        writable_delegated,
        writable_delegated_owner,
        fetchable_at_slot,
    );

    let manager = setup_standard(
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer,
        account_updates,
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

    assert!(!manager.account_cloner.did_clone(&readonly_already_loaded));
    assert!(manager.account_cloner.did_clone(&writable_delegated));

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
    let mut account_fetcher = AccountFetcherStub::default();
    let account_cloner = AccountClonerStub::default();
    let account_committer = AccountCommitterStub::default();
    let account_updates = AccountUpdatesStub::default();

    let fetchable_at_slot = 45;
    account_fetcher.add_delegated(
        writable_delegated,
        writable_delegated_owner,
        fetchable_at_slot,
    );
    account_fetcher
        .add_undelegated(writable_undelegated_payer, fetchable_at_slot);

    let manager = setup_standard(
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer,
        account_updates,
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

    assert!(manager.account_cloner.did_clone(&writable_delegated));
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
    let mut account_fetcher = AccountFetcherStub::default();
    let account_cloner = AccountClonerStub::default();
    let account_committer = AccountCommitterStub::default();
    let account_updates = AccountUpdatesStub::default();

    let fetchable_at_slot = 42;
    account_fetcher.add_delegated(
        writable_delegated,
        Pubkey::new_unique(),
        fetchable_at_slot,
    );

    // Note: since we use a writable new account, we need to allow it as part of the configuration
    let manager = setup_customized(
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer,
        account_updates,
        ExternalReadonlyMode::All,
        ExternalWritableMode::All,
        true,
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

    assert!(manager.account_cloner.did_clone(&writable_delegated));
    assert!(!manager.account_cloner.did_clone(&writable_new_account));

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
    let mut account_fetcher = AccountFetcherStub::default();
    let account_cloner = AccountClonerStub::default();
    let account_committer = AccountCommitterStub::default();
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

    let manager = setup_standard(
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer,
        account_updates,
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

        assert!(manager.account_cloner.did_clone(&readonly1_undelegated));
        assert!(manager.account_cloner.did_clone(&readonly2_undelegated));
        assert!(!manager.account_cloner.did_clone(&readonly3_undelegated));

        assert!(manager.account_cloner.did_clone(&writable1_delegated));
        assert!(!manager.account_cloner.did_clone(&writable2_delegated));

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

        assert!(!manager.account_cloner.did_clone(&readonly1_undelegated));
        assert!(!manager.account_cloner.did_clone(&readonly2_undelegated));
        assert!(!manager.account_cloner.did_clone(&readonly3_undelegated));

        assert!(!manager.account_cloner.did_clone(&writable1_delegated));
        assert!(!manager.account_cloner.did_clone(&writable2_delegated));

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

        assert!(!manager.account_cloner.did_clone(&readonly1_undelegated));
        assert!(!manager.account_cloner.did_clone(&readonly2_undelegated));
        assert!(manager.account_cloner.did_clone(&readonly3_undelegated));

        assert!(!manager.account_cloner.did_clone(&writable1_delegated));
        assert!(manager.account_cloner.did_clone(&writable2_delegated));

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
    let account_cloner = AccountClonerStub::default();
    let account_committer = AccountCommitterStub::default();
    let account_updates = AccountUpdatesStub::default();

    let manager = setup_standard(
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer,
        account_updates,
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
    let mut account_fetcher = AccountFetcherStub::default();
    let account_cloner = AccountClonerStub::default();
    let account_committer = AccountCommitterStub::default();
    let account_updates = AccountUpdatesStub::default();

    let fetchable_at_slot = 42;
    account_fetcher.add_delegated(
        account_delegated,
        account_delegated_owner,
        fetchable_at_slot,
    );

    let manager = setup_standard(
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer,
        account_updates,
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

        assert!(manager.account_cloner.did_clone(&account_delegated));

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
        assert!(manager.account_cloner.did_clone(&account_delegated));

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

        assert!(!manager.account_cloner.did_clone(&account_delegated));

        assert!(manager.external_readonly_accounts.is_empty());
        assert!(manager.external_writable_accounts.has(&account_delegated));
    }
}

#[tokio::test]
async fn test_ensure_accounts_already_known_can_be_reused_as_writable_later() {
    init_logger!();
    let account_delegated = Pubkey::new_unique();
    let account_delegated_owner = Pubkey::new_unique();

    let mut internal_account_provider = InternalAccountProviderStub::default();
    let mut account_fetcher = AccountFetcherStub::default();
    let account_cloner = AccountClonerStub::default();
    let account_committer = AccountCommitterStub::default();
    let account_updates = AccountUpdatesStub::default();

    internal_account_provider.add(account_delegated, Default::default());

    let fetchable_at_slot = 42;
    account_fetcher.add_delegated(
        account_delegated,
        account_delegated_owner,
        fetchable_at_slot,
    );

    let manager = setup_standard(
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer,
        account_updates,
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

        assert!(!manager.account_cloner.did_clone(&account_delegated));

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

        assert!(manager.account_cloner.did_clone(&account_delegated));

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

    let mut internal_account_provider = InternalAccountProviderStub::default();
    let mut account_fetcher = AccountFetcherStub::default();
    let account_cloner = AccountClonerStub::default();
    let account_committer = AccountCommitterStub::default();
    let mut account_updates = AccountUpdatesStub::default();

    let cloned_at_slot = 11;
    let fetchable_at_slot = 14;
    let last_updated_at_slot = 42;

    internal_account_provider.add(account_undelegated, Default::default());
    account_fetcher.add_undelegated(account_undelegated, fetchable_at_slot);
    account_updates.add_known_update(account_undelegated, last_updated_at_slot);

    let manager = setup_standard(
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer,
        account_updates,
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

        assert!(manager.account_cloner.did_clone(&account_undelegated));

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

        assert!(manager.account_cloner.did_clone(&account_undelegated));

        assert!(manager.external_readonly_accounts.has(&account_undelegated));
        assert!(manager.external_writable_accounts.is_empty());
    }
}

#[tokio::test]
async fn test_ensure_accounts_already_known_can_be_reused_without_updates() {
    init_logger!();
    let account_undelegated = Pubkey::new_unique();

    let mut internal_account_provider = InternalAccountProviderStub::default();
    let mut account_fetcher = AccountFetcherStub::default();
    let account_cloner = AccountClonerStub::default();
    let account_committer = AccountCommitterStub::default();
    let mut account_updates = AccountUpdatesStub::default();

    let cloned_at_slot = 11;
    let last_updated_at_slot = 15;
    let fetchable_at_slot = 20;

    internal_account_provider.add(account_undelegated, Default::default());
    account_fetcher.add_undelegated(account_undelegated, fetchable_at_slot);
    account_updates.add_known_update(account_undelegated, last_updated_at_slot);

    let manager = setup_standard(
        internal_account_provider,
        account_fetcher,
        account_cloner,
        account_committer,
        account_updates,
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

        assert!(manager.account_cloner.did_clone(&account_undelegated));

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

        assert!(!manager.account_cloner.did_clone(&account_undelegated));

        assert!(manager.external_readonly_accounts.has(&account_undelegated));
        assert!(manager.external_writable_accounts.is_empty());
    }
}
