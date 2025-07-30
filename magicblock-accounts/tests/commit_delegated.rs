use std::sync::Arc;

use conjunto_transwise::{
    transaction_accounts_extractor::TransactionAccountsExtractorImpl,
    transaction_accounts_holder::TransactionAccountsHolder,
    transaction_accounts_validator::TransactionAccountsValidatorImpl,
    AccountChainSnapshot, AccountChainSnapshotShared, AccountChainState,
    CommitFrequency, DelegationRecord,
};
use magicblock_account_cloner::{AccountClonerOutput, AccountClonerStub};
use magicblock_accounts::{ExternalAccountsManager, LifecycleMode};
use magicblock_accounts_api::InternalAccountProviderStub;
use magicblock_committor_service::stubs::ChangesetCommittorStub;
use magicblock_program::validator::generate_validator_authority_if_needed;
use solana_sdk::{
    account::{Account, AccountSharedData},
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::Signature,
};
use stubs::scheduled_commits_processor_stub::ScheduledCommitsProcessorStub;
use test_tools_core::init_logger;

mod stubs;

type StubbedAccountsManager = ExternalAccountsManager<
    InternalAccountProviderStub,
    AccountClonerStub,
    TransactionAccountsExtractorImpl,
    TransactionAccountsValidatorImpl,
    ChangesetCommittorStub,
>;

fn setup(
    internal_account_provider: InternalAccountProviderStub,
    account_cloner: AccountClonerStub,
    committor_service: Arc<ChangesetCommittorStub>,
) -> StubbedAccountsManager {
    ExternalAccountsManager {
        internal_account_provider,
        account_cloner,
        transaction_accounts_extractor: TransactionAccountsExtractorImpl,
        transaction_accounts_validator: TransactionAccountsValidatorImpl,
        committor_service,
        lifecycle: LifecycleMode::Ephemeral,
        external_commitable_accounts: Default::default(),
    }
}

fn generate_account(pubkey: &Pubkey) -> Account {
    Account {
        lamports: 1_000 * LAMPORTS_PER_SOL,
        // Account owns itself for simplicity, just so we can identify them
        // via an equality check
        owner: *pubkey,
        data: vec![],
        executable: false,
        rent_epoch: 0,
    }
}
fn generate_delegated_account_chain_snapshot(
    pubkey: &Pubkey,
    account: &Account,
    commit_frequency: CommitFrequency,
) -> AccountChainSnapshotShared {
    AccountChainSnapshot {
        pubkey: *pubkey,
        at_slot: 42,
        chain_state: AccountChainState::Delegated {
            account: account.clone(),
            delegation_record: DelegationRecord {
                authority: Pubkey::new_unique(),
                owner: account.owner,
                delegation_slot: 42,
                lamports: 100,
                commit_frequency,
            },
        },
    }
    .into()
}

#[tokio::test]
async fn test_commit_two_delegated_accounts_one_needs_commit() {
    init_logger!();

    generate_validator_authority_if_needed();
    let commit_needed_pubkey = Pubkey::new_unique();
    let commit_needed_account = generate_account(&commit_needed_pubkey);
    let commit_needed_account_shared =
        AccountSharedData::from(commit_needed_account.clone());

    let commit_not_needed_pubkey = Pubkey::new_unique();
    let commit_not_needed_account = generate_account(&commit_not_needed_pubkey);
    let commit_not_needed_account_shared =
        AccountSharedData::from(commit_not_needed_account.clone());

    let internal_account_provider = InternalAccountProviderStub::default();
    let account_cloner = AccountClonerStub::default();
    let committor_service = Arc::new(ChangesetCommittorStub::default());

    let manager = setup(
        internal_account_provider.clone(),
        account_cloner.clone(),
        committor_service.clone(),
    );

    // Clone the accounts through a dummy transaction
    account_cloner.set(
        &commit_needed_pubkey,
        AccountClonerOutput::Cloned {
            account_chain_snapshot: generate_delegated_account_chain_snapshot(
                &commit_needed_pubkey,
                &commit_needed_account,
                CommitFrequency::Millis(1),
            ),
            signature: Signature::new_unique(),
        },
    );
    account_cloner.set(
        &commit_not_needed_pubkey,
        AccountClonerOutput::Cloned {
            account_chain_snapshot: generate_delegated_account_chain_snapshot(
                &commit_not_needed_pubkey,
                &commit_not_needed_account,
                CommitFrequency::Millis(60_000),
            ),
            signature: Signature::new_unique(),
        },
    );
    let result = manager
        .ensure_accounts_from_holder(
            TransactionAccountsHolder {
                readonly: vec![commit_needed_pubkey, commit_not_needed_pubkey],
                writable: vec![],
                payer: Pubkey::new_unique(),
            },
            "tx-sig".to_string(),
        )
        .await;
    assert!(result.is_ok());

    // Once the accounts are cloned, make sure they've been added to the bank (Stubbed dumper doesn't do anything)
    internal_account_provider
        .set(commit_needed_pubkey, commit_needed_account_shared.clone());
    internal_account_provider
        .set(commit_not_needed_pubkey, commit_not_needed_account_shared);

    // Since accounts are delegated, we should have initialized the commit timestamp
    let last_commit_of_commit_needed =
        manager.last_commit(&commit_needed_pubkey).unwrap();
    let last_commit_of_commit_not_needed =
        manager.last_commit(&commit_not_needed_pubkey).unwrap();

    // Wait for one of the commit's frequency to be triggered
    tokio::time::sleep(tokio::time::Duration::from_millis(2)).await;

    // Execute the commits of the accounts that needs it
    let result = manager.commit_delegated().await;
    // Ensure we committed the account that was due
    assert_eq!(committor_service.len(), 1);
    // and that we returned that transaction signature for it.
    assert_eq!(result.unwrap().len(), 1);

    // Ensure that the last commit time was updated of the committed account
    assert!(
        manager.last_commit(&commit_needed_pubkey).unwrap()
            > last_commit_of_commit_needed
    );
    // but not of the one that didn't need commit.
    assert_eq!(
        manager.last_commit(&commit_not_needed_pubkey).unwrap(),
        last_commit_of_commit_not_needed
    );
}
