use conjunto_transwise::{
    errors::TranswiseError, trans_account_meta::TransactionAccountsHolder,
};
use sleipnir_accounts::{
    errors::AccountsError, ExternalAccountsManager, ExternalReadonlyMode,
    ExternalWritableMode,
};
use solana_sdk::pubkey::Pubkey;
use test_tools_core::init_logger;
use utils::stubs::{
    AccountClonerStub, InternalAccountProviderStub,
    ValidatedAccountsProviderStub,
};

mod utils;

fn setup(
    internal_account_provider: InternalAccountProviderStub,
    account_cloner: AccountClonerStub,
    validated_accounts_provider: ValidatedAccountsProviderStub,
) -> ExternalAccountsManager<
    InternalAccountProviderStub,
    AccountClonerStub,
    ValidatedAccountsProviderStub,
> {
    ExternalAccountsManager {
        internal_account_provider,
        account_cloner,
        validated_accounts_provider,
        external_readonly_accounts: Default::default(),
        external_writable_accounts: Default::default(),
        external_readonly_mode: ExternalReadonlyMode::All,
        external_writable_mode: ExternalWritableMode::Delegated,
        create_accounts: false,
    }
}

#[tokio::test]
async fn test_ensure_readonly_account_not_tracked_nor_in_our_validator() {
    init_logger!();
    let readonly = Pubkey::new_unique();

    let internal_account_provider = InternalAccountProviderStub::default();
    let validated_accounts_provider = ValidatedAccountsProviderStub::valid();

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        validated_accounts_provider,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly],
        writable: vec![],
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
    let validated_accounts_provider = ValidatedAccountsProviderStub::valid();

    internal_account_provider.add(readonly, Default::default());

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        validated_accounts_provider,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly],
        writable: vec![],
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
    let validated_accounts_provider = ValidatedAccountsProviderStub::valid();

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        validated_accounts_provider,
    );

    manager.external_readonly_accounts.insert(readonly);

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly],
        writable: vec![],
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
    let validated_accounts_provider = ValidatedAccountsProviderStub::valid();

    internal_account_provider.add(readonly, Default::default());

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        validated_accounts_provider,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![readonly],
        writable: vec![writable],
    };

    let result = manager
        .ensure_accounts_from_holder(holder, "tx-sig".to_string())
        .await;
    assert_eq!(result.unwrap().len(), 1);
    assert!(!manager.account_cloner.did_clone(&readonly));
    assert!(manager.account_cloner.did_clone(&writable));
    assert!(manager.external_readonly_accounts.is_empty());
    assert!(manager.external_writable_accounts.has(&writable));
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
    let validated_accounts_provider = ValidatedAccountsProviderStub::valid();

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        validated_accounts_provider,
    );

    // First Transaction
    {
        let holder = TransactionAccountsHolder {
            readonly: vec![readonly1, readonly2],
            writable: vec![writable1],
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
            new_accounts: vec![writable],
        },
    );

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        validated_accounts_provider,
    );

    let holder = TransactionAccountsHolder {
        readonly: vec![],
        writable: vec![writable],
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
