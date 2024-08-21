use std::sync::Arc;

use async_trait::async_trait;
use sleipnir_mutator::AccountModification;
use solana_sdk::{
    account::{Account, AccountSharedData},
    pubkey::Pubkey,
    signature::Signature,
    transaction::Transaction,
};

use crate::errors::AccountsResult;

#[async_trait]
pub trait ScheduledCommitsProcessor {
    async fn process<AC: AccountCommitter, IAP: InternalAccountProvider>(
        &self,
        committer: &Arc<AC>,
        account_provider: &IAP,
    ) -> AccountsResult<()>;
}

pub trait InternalAccountProvider: Send + Sync {
    fn has_account(&self, pubkey: &Pubkey) -> bool;
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData>;
    fn get_slot(&self) -> u64;
}

#[async_trait]
pub trait AccountCloner {
    async fn clone_account(
        &self,
        pubkey: &Pubkey,
        account: Option<Account>,
        overrides: Option<AccountModification>,
    ) -> AccountsResult<Signature>;
}

#[derive(Clone)]
pub struct UndelegationRequest {
    /// The original owner of the account before it was delegated.
    pub owner: Pubkey,
}

pub struct AccountCommittee {
    /// The pubkey of the account to be committed.
    pub pubkey: Pubkey,
    /// The current account state.
    /// NOTE: if undelegation was requested the owner is set to the
    /// delegation program when accounts are committed.
    pub account_data: AccountSharedData,
    /// Slot at which the commit was scheduled.
    pub slot: u64,
    /// Only present if undelegation was requested.
    pub undelegation_request: Option<UndelegationRequest>,
}

pub struct CommitAccountsPayload {
    /// The transaction that commits the accounts.
    /// None if no accounts need to be committed.
    pub transaction: Option<Transaction>,
    /// The pubkeys and data of the accounts that were committed.
    pub committees: Vec<(Pubkey, AccountSharedData)>,
}

/// Same as [CommitAccountsPayload] but one that is actionable
pub struct SendableCommitAccountsPayload {
    pub transaction: Transaction,
    /// The pubkeys and data of the accounts that were committed.
    pub committees: Vec<(Pubkey, AccountSharedData)>,
}

#[async_trait]
pub trait AccountCommitter: Send + Sync + 'static {
    /// Creates a transaction to commit each provided account unless it determines
    /// that it isn't necessary, i.e. when the previously committed state is the same
    /// as the [commit_state_data].
    /// Returns the transaction committing the accounts and the pubkeys of accounts
    /// it did commit
    async fn create_commit_accounts_transactions(
        &self,
        committees: Vec<AccountCommittee>,
    ) -> AccountsResult<Vec<CommitAccountsPayload>>;

    /// Returns the main-chain signatures of the commit transactions
    /// This will only fail due to network issues, not if the transaction failed.
    /// Therefore we want to either fail all transactions or none which is why
    /// we return a `Result<Vec>` instead of a `Vec<Result>`.
    async fn send_commit_transactions(
        &self,
        payloads: Vec<SendableCommitAccountsPayload>,
    ) -> AccountsResult<Vec<Signature>>;
}
