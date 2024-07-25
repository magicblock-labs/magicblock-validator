use conjunto_transwise::{CommitFrequency, TransactionAccountsExtractorImpl};
use sleipnir_accounts::{
    ExternalAccountsManager, ExternalReadonlyMode, ExternalWritableMode,
};
use solana_sdk::{
    account::{Account, AccountSharedData},
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
};
use stubs::{
    account_cloner_stub::AccountClonerStub,
    account_committer_stub::AccountCommitterStub,
    internal_account_provider_stub::InternalAccountProviderStub,
    validated_accounts_provider_stub::ValidatedAccountsProviderStub,
};
use test_tools_core::init_logger;

mod stubs;

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
> {
    ExternalAccountsManager {
        internal_account_provider,
        account_cloner,
        account_committer,
        validated_accounts_provider,
        transaction_accounts_extractor: TransactionAccountsExtractorImpl,
        external_readonly_accounts: Default::default(),
        external_writable_accounts: Default::default(),
        external_readonly_mode: ExternalReadonlyMode::All,
        external_writable_mode: ExternalWritableMode::Delegated,
        create_accounts: false,
        payer_init_lamports: Some(1_000 * LAMPORTS_PER_SOL),
    }
}

fn acount_shared_data(pubkey: Pubkey) -> AccountSharedData {
    AccountSharedData::from(Account {
        lamports: 1_000 * LAMPORTS_PER_SOL,
        // Account owns itself for simplicity, just so we can identify them
        // via an equality check
        owner: pubkey,
        data: vec![],
        executable: false,
        rent_epoch: 0,
    })
}

#[tokio::test]
async fn test_commit_two_delegated_accounts_one_needs_commit() {
    init_logger!();

    let commit_needed = Pubkey::new_unique();
    let commit_needed_acc = acount_shared_data(commit_needed);
    let commit_not_needed = Pubkey::new_unique();
    let commit_not_needed_acc = acount_shared_data(commit_not_needed);

    let mut internal_account_provider = InternalAccountProviderStub::default();
    internal_account_provider.add(commit_needed, commit_needed_acc.clone());
    internal_account_provider.add(commit_not_needed, commit_not_needed_acc);

    let account_committer = AccountCommitterStub::default();

    let manager = setup(
        internal_account_provider,
        AccountClonerStub::default(),
        account_committer.clone(),
        ValidatedAccountsProviderStub::valid_default(),
    );

    manager
        .external_writable_accounts
        .insert(commit_needed, Some(CommitFrequency::Millis(1)));

    manager
        .external_writable_accounts
        .insert(commit_not_needed, Some(CommitFrequency::Millis(60_000)));

    let last_commit_of_commit_needed =
        manager.last_commit(&commit_needed).unwrap();
    let last_commit_of_commit_not_needed =
        manager.last_commit(&commit_not_needed).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(2));

    let result = manager.commit_delegated().await;
    // Ensure we committed the account that was due
    assert_eq!(account_committer.len(), 1);
    // with the current account data
    assert_eq!(
        account_committer.committed(&commit_needed),
        Some(commit_needed_acc)
    );
    // and that we returned that transaction signature for it.
    assert_eq!(result.unwrap().len(), 1);

    // Ensure that the last commit time was updated of the committed account
    assert!(
        manager.last_commit(&commit_needed).unwrap()
            > last_commit_of_commit_needed
    );
    // but not of the one that didn't need commit.
    assert_eq!(
        manager.last_commit(&commit_not_needed).unwrap(),
        last_commit_of_commit_not_needed
    );
}
