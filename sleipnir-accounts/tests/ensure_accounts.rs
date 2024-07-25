use std::sync::Arc;

use conjunto_transwise::{
    errors::TranswiseError,
    transaction_accounts_holder::TransactionAccountsHolder,
    TransactionAccountsExtractorImpl,
};
use sleipnir_accounts::{
    errors::AccountsError, ExternalAccountsManager, ExternalReadonlyMode,
    ExternalWritableMode,
};
use solana_sdk::{native_token::LAMPORTS_PER_SOL, pubkey::Pubkey};
use test_tools_core::init_logger;
use utils::stubs::{
    AccountClonerStub, AccountCommitterStub, InternalAccountProviderStub,
    ScheduledCommitsProcessorStub, ValidatedAccountsProviderStub,
};

mod utils;

fn setup(
    internal_account_provider: InternalAccountProviderStub,
    account_cloner: AccountClonerStub,
    account_committer: AccountCommitterStub,
    validated_accounts_provider: ValidatedAccountsProviderStub,
) -> ExternalAccountsManager<
    InternalAccountProviderStub,
    AccountClonerStub,
    AccountCommitterStub,
    ValidatedAccountsProviderStub,
    TransactionAccountsExtractorImpl,
    ScheduledCommitsProcessorStub,
> {
    ExternalAccountsManager {
        internal_account_provider,
        account_cloner,
        account_committer: Arc::new(account_committer),
        validated_accounts_provider,
        transaction_accounts_extractor: TransactionAccountsExtractorImpl,
        external_readonly_accounts: Default::default(),
        external_writable_accounts: Default::default(),
        scheduled_commits_processor: ScheduledCommitsProcessorStub::default(),
        external_readonly_mode: ExternalReadonlyMode::All,
        external_writable_mode: ExternalWritableMode::Delegated,
        create_accounts: false,
        payer_init_lamports: Some(1_000 * LAMPORTS_PER_SOL),
    }
}

#[tokio::test]
async fn test_ensure_readonly_account_not_tracked_nor_in_our_validator() {
    init_logger!();
    let readonly = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let validated_accounts_provider =
        ValidatedAccountsProviderStub::valid_default();

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        AccountCommitterStub::default(),
        validated_accounts_provider,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly],
        writable: vec![],
        payer: Pubkey::new_unique(),
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;

    assert_eq!(result.unwrap().len(), 1);
    assert!(manager.account_cloner.did_clone(&readonly));
    assert!(manager.external_readonly_accounts.has(&readonly));
    assert!(manager.external_writable_accounts.is_empty());
}

#[tokio::test]
async fn test_ensure_readonly_account_not_tracked_but_in_our_validator() {
    init_logger!();
    let readonly = Pubkey::new_unique();

    let mut internal_account_provider = InternalAccountProviderStub::default();
    let validated_accounts_provider =
        ValidatedAccountsProviderStub::valid_default();

    internal_account_provider.add(readonly, Default::default());

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        AccountCommitterStub::default(),
        validated_accounts_provider,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly],
        writable: vec![],
        payer: Pubkey::new_unique(),
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;

    assert_eq!(result.unwrap().len(), 0);
    assert!(!manager.account_cloner.did_clone(&readonly));
    assert!(manager.external_readonly_accounts.is_empty());
    assert!(manager.external_writable_accounts.is_empty());
}

#[tokio::test]
async fn test_ensure_readonly_account_tracked_but_not_in_our_validator() {
    init_logger!();
    let readonly = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let validated_accounts_provider =
        ValidatedAccountsProviderStub::valid_default();

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        AccountCommitterStub::default(),
        validated_accounts_provider,
    );

    manager.external_readonly_accounts.insert(readonly);

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly],
        writable: vec![],
        payer: Pubkey::new_unique(),
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;

    assert_eq!(result.unwrap().len(), 0);
    assert!(!manager.account_cloner.did_clone(&readonly));
    assert_eq!(manager.external_readonly_accounts.len(), 1);
    assert!(manager.external_writable_accounts.is_empty());
}

#[tokio::test]
async fn test_ensure_readonly_account_in_our_validator_and_new_writable() {
    init_logger!();
    let readonly = Pubkey::new_unique();
    let writable = Pubkey::new_unique();

    let mut internal_account_provider = InternalAccountProviderStub::default();
    let validated_accounts_provider =
        ValidatedAccountsProviderStub::valid_default();

    internal_account_provider.add(readonly, Default::default());

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        AccountCommitterStub::default(),
        validated_accounts_provider,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly],
        writable: vec![writable],
        payer: Pubkey::new_unique(),
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;
    assert_eq!(result.unwrap().len(), 1);
    assert!(!manager.account_cloner.did_clone(&readonly));
    assert!(manager.account_cloner.did_clone(&writable));
    assert!(manager.account_cloner.did_not_override_lamports(&writable));
    assert!(manager.external_readonly_accounts.is_empty());
    assert!(manager.external_writable_accounts.has(&writable));
}

#[tokio::test]
async fn test_ensure_locked_with_owner_and_unlocked_writable_payer() {
    init_logger!();
    let locked = Pubkey::new_unique();
    let locked_owner = Pubkey::new_unique();
    let payer = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let payers = vec![payer].into_iter().collect();
    let with_owners = vec![(locked, locked_owner)].into_iter().collect();

    let validated_accounts_provider = ValidatedAccountsProviderStub::valid(
        payers,
        Default::default(),
        with_owners,
    );

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        AccountCommitterStub::default(),
        validated_accounts_provider,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![],
        writable: vec![payer, locked],
        payer,
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;
    assert_eq!(result.unwrap().len(), 2);

    assert!(manager.external_readonly_accounts.is_empty());
    assert!(manager.external_writable_accounts.has(&payer));
    assert!(manager.external_writable_accounts.has(&locked));

    assert!(manager.account_cloner.did_clone(&payer));
    assert!(manager
        .account_cloner
        .did_override_lamports(&payer, LAMPORTS_PER_SOL * 1_000));
    assert!(manager.account_cloner.did_not_override_owner(&payer));

    assert!(manager
        .account_cloner
        .did_override_owner(&locked, &locked_owner));
    assert!(manager.account_cloner.did_not_override_lamports(&locked));
}

#[tokio::test]
async fn test_ensure_one_locked_and_one_new_writable() {
    init_logger!();
    let locked = Pubkey::new_unique();
    let new = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let new_accounts = vec![new].into_iter().collect();

    let validated_accounts_provider = ValidatedAccountsProviderStub::valid(
        Default::default(),
        new_accounts,
        Default::default(),
    );

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        AccountCommitterStub::default(),
        validated_accounts_provider,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![],
        writable: vec![new, locked],
        payer: Pubkey::new_unique(),
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;
    assert_eq!(result.unwrap().len(), 1);

    assert!(manager.external_readonly_accounts.is_empty());
    assert_eq!(manager.external_writable_accounts.len(), 1);
    assert!(manager.external_writable_accounts.has(&locked));
    assert!(!manager.external_writable_accounts.has(&new));

    assert!(manager.account_cloner.did_clone(&locked));
    assert!(!manager.account_cloner.did_clone(&new));
}

#[tokio::test]
async fn test_ensure_multiple_accounts_coming_in_over_time() {
    init_logger!();
    let readonly1 = Pubkey::new_unique();
    let readonly2 = Pubkey::new_unique();
    let readonly3 = Pubkey::new_unique();
    let writable1 = Pubkey::new_unique();
    let writable2 = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let validated_accounts_provider =
        ValidatedAccountsProviderStub::valid_default();

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        AccountCommitterStub::default(),
        validated_accounts_provider,
    );

    // First Transaction
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![readonly1, readonly2],
            writable: vec![writable1],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;
        assert_eq!(result.unwrap().len(), 3);

        assert!(manager.account_cloner.did_clone(&readonly1));
        assert!(manager.account_cloner.did_clone(&readonly2));
        assert!(!manager.account_cloner.did_clone(&readonly3));
        assert!(manager.account_cloner.did_clone(&writable1));
        assert!(!manager.account_cloner.did_clone(&writable2));

        assert!(manager.external_readonly_accounts.has(&readonly1));
        assert!(manager.external_readonly_accounts.has(&readonly2));
        assert!(!manager.external_readonly_accounts.has(&readonly3));
        assert!(manager.external_writable_accounts.has(&writable1));
        assert!(!manager.external_writable_accounts.has(&writable2));
    }

    manager.account_cloner.clear();

    // Second Transaction
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![readonly1, readonly2],
            writable: vec![],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;
        assert!(result.unwrap().is_empty());

        assert!(!manager.account_cloner.did_clone(&readonly1));
        assert!(!manager.account_cloner.did_clone(&readonly2));
        assert!(!manager.account_cloner.did_clone(&readonly3));
        assert!(!manager.account_cloner.did_clone(&writable1));
        assert!(!manager.account_cloner.did_clone(&writable2));

        assert!(manager.external_readonly_accounts.has(&readonly1));
        assert!(manager.external_readonly_accounts.has(&readonly2));
        assert!(!manager.external_readonly_accounts.has(&readonly3));
        assert!(manager.external_writable_accounts.has(&writable1));
        assert!(!manager.external_writable_accounts.has(&writable2));
    }

    manager.account_cloner.clear();

    // Third Transaction
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![readonly2, readonly3],
            writable: vec![writable2],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;
        assert_eq!(result.unwrap().len(), 2);

        assert!(!manager.account_cloner.did_clone(&readonly1));
        assert!(!manager.account_cloner.did_clone(&readonly2));
        assert!(manager.account_cloner.did_clone(&readonly3));
        assert!(!manager.account_cloner.did_clone(&writable1));
        assert!(manager.account_cloner.did_clone(&writable2));

        assert!(manager.external_readonly_accounts.has(&readonly1));
        assert!(manager.external_readonly_accounts.has(&readonly2));
        assert!(manager.external_readonly_accounts.has(&readonly3));
        assert!(manager.external_writable_accounts.has(&writable1));
        assert!(manager.external_writable_accounts.has(&writable2));
    }
}

#[tokio::test]
async fn test_ensure_writable_account_fails_to_validate() {
    init_logger!();
    let writable = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let validated_accounts_provider = ValidatedAccountsProviderStub::invalid(
        TranswiseError::WritablesIncludeNewAccounts {
            writable_new_pubkeys: vec![writable],
        },
    );

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        AccountCommitterStub::default(),
        validated_accounts_provider,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![],
        writable: vec![writable],
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
    let account = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let validated_accounts_provider =
        ValidatedAccountsProviderStub::valid_default();

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        AccountCommitterStub::default(),
        validated_accounts_provider,
    );

    // First Transaction uses the account as a readable
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![account],
            writable: vec![],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;

        assert_eq!(result.unwrap().len(), 1);

        assert!(manager.account_cloner.did_clone(&account));

        assert!(manager.external_readonly_accounts.has(&account));
        assert!(manager.external_writable_accounts.is_empty());
    }

    manager.account_cloner.clear();

    // Second Transaction uses the same account as a writable
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![],
            writable: vec![account],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;

        assert_eq!(result.unwrap().len(), 1);

        assert!(manager.account_cloner.did_clone(&account));

        assert!(manager.external_readonly_accounts.is_empty());
        assert!(manager.external_writable_accounts.has(&account));
    }

    manager.account_cloner.clear();

    // Third transaction reuse the account as readable, nothing should happen then
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![account],
            writable: vec![],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;

        assert_eq!(result.unwrap().len(), 0);

        assert!(!manager.account_cloner.did_clone(&account));

        assert!(manager.external_readonly_accounts.is_empty());
        assert!(manager.external_writable_accounts.has(&account));
    }
}

#[tokio::test]
async fn test_ensure_accounts_already_known_can_be_reused_as_writable_later() {
    init_logger!();
    let account = Pubkey::new_unique();

    let mut internal_account_provider = InternalAccountProviderStub::default();
    let validated_accounts_provider =
        ValidatedAccountsProviderStub::valid_default();

    internal_account_provider.add(account, Default::default());

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        AccountCommitterStub::default(),
        validated_accounts_provider,
    );

    // First Transaction does not need to re-clone account to use it as readonly
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![account],
            writable: vec![],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;

        assert_eq!(result.unwrap().len(), 0);

        assert!(!manager.account_cloner.did_clone(&account));

        assert!(manager.external_readonly_accounts.is_empty());
        assert!(manager.external_writable_accounts.is_empty());
    }

    manager.account_cloner.clear();

    // Second Transaction does need to re-clone account to override it, so it can be used as a writable
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![],
            writable: vec![account],
            payer: Pubkey::new_unique(),
        };

        let result = manager
            .ensure_accounts_from_holder(holder, "tx-sig".to_string())
            .await;

        assert_eq!(result.unwrap().len(), 1);

        assert!(manager.account_cloner.did_clone(&account));

        assert!(manager.external_readonly_accounts.is_empty());
        assert!(manager.external_writable_accounts.has(&account));
    }
}
