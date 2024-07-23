use async_trait::async_trait;
use sleipnir_mutator::AccountModification;
use solana_sdk::{
    account::{Account, AccountSharedData},
    pubkey::Pubkey,
    signature::Signature,
    transaction::Transaction,
};

use crate::errors::AccountsResult;

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

#[async_trait]
pub trait AccountCommitter {
    /// Creates a transaction to commit the account unless it determines that it
    /// isn't necessary, i.e. when the previously committed state is the same
    /// as the [commit_state_data].
    async fn create_commit_account_transaction(
        &self,
        delegated_account: Pubkey,
        commit_state_data: AccountSharedData,
    ) -> AccountsResult<Option<Transaction>>;

    /// Returns the signature of the commit transaction if it was committed,
    /// otherwise [None].
    async fn commit_account(
        &self,
        delegated_account: Pubkey,
        commit_state_data: AccountSharedData,
        transaction: Transaction,
    ) -> AccountsResult<Signature>;
}

pub trait AccountUpdates {
    fn ensure_monitoring_of_account(&self, pubkey: &Pubkey);
    fn has_known_update_since_slot(&self, pubkey: &Pubkey, slot: u64) -> bool;
}
