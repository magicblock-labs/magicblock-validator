use async_trait::async_trait;
use sleipnir_mutator::AccountModification;
use solana_sdk::{
    account::{Account, AccountSharedData},
    hash::Hash,
    pubkey::Pubkey,
    signature::Signature,
    transaction::Transaction,
};

use crate::errors::AccountsResult;

#[async_trait]
pub trait ScheduledCommitsProcessor {
    async fn process(&self) -> AccountsResult<()>;
}

pub trait InternalAccountProvider {
    fn has_account(&self, pubkey: &Pubkey) -> bool;
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData>;
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

pub struct AccountCommittee {
    pub pubkey: Pubkey,
    pub account_data: AccountSharedData,
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
pub trait AccountCommitter {
    /// Creates a transaction to commit each provided account unless it determines
    /// that it isn't necessary, i.e. when the previously committed state is the same
    /// as the [commit_state_data].
    /// Returns the transaction committing the accounts and the pubkeys of accounts
    /// it did commit
    async fn create_commit_accounts_transactions(
        &self,
        committees: Vec<AccountCommittee>,
        latest_blockhash: Option<Hash>,
    ) -> AccountsResult<Vec<CommitAccountsPayload>>;

    /// Returns the signaturse of the commit transactions processed.
    async fn send_commit_transactions(
        &self,
        payloads: Vec<SendableCommitAccountsPayload>,
    ) -> AccountsResult<Vec<Signature>>;
}
