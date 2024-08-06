use std::sync::Arc;

use conjunto_transwise::{
    transaction_accounts_extractor::TransactionAccountsExtractorImpl,
    transaction_accounts_validator::TransactionAccountsValidatorImpl,
    CommitFrequency,
};
use sleipnir_accounts::{ExternalAccountsManager, ExternalCloneMode};
use solana_sdk::{
    account::{Account, AccountSharedData},
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
};
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

fn setup(
    internal_account_provider: InternalAccountProviderStub,
    account_fetcher: AccountFetcherStub,
    account_cloner: AccountClonerStub,
    account_committer: AccountCommitterStub,
    account_updates: AccountUpdatesStub,
    validator_auth_id: Pubkey,
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
    ExternalAccountsManager {
        internal_account_provider,
        account_fetcher,
        account_committer: Arc::new(account_committer),
        account_updates,
        account_cloner,
        transaction_accounts_extractor: TransactionAccountsExtractorImpl,
        transaction_accounts_validator: TransactionAccountsValidatorImpl,
        scheduled_commits_processor: ScheduledCommitsProcessorStub::default(),
        external_readonly_accounts: Default::default(),
        external_writable_accounts: Default::default(),
        external_clone_mode: ExternalCloneMode::Everything,
        create_accounts: false,
        payer_init_lamports: Some(1_000 * LAMPORTS_PER_SOL),
        validator_id: validator_auth_id,
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
    let validator_auth_id = Pubkey::new_unique();

    let manager = setup(
        internal_account_provider,
        AccountFetcherStub::default(),
        AccountClonerStub::default(),
        account_committer.clone(),
        AccountUpdatesStub::default(),
        validator_auth_id,
    );

    let cloned_at_slot = 12;

    manager.external_writable_accounts.insert(
        commit_needed,
        cloned_at_slot,
        Some(CommitFrequency::Millis(1)),
    );

    manager.external_writable_accounts.insert(
        commit_not_needed,
        cloned_at_slot,
        Some(CommitFrequency::Millis(60_000)),
    );

    let last_commit_of_commit_needed =
        manager.last_commit(&commit_needed).unwrap();
    let last_commit_of_commit_not_needed =
        manager.last_commit(&commit_not_needed).unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(2)).await;

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
