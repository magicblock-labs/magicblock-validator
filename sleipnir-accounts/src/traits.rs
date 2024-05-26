use async_trait::async_trait;
use sleipnir_mutator::AccountModification;
use solana_sdk::{
    account::AccountSharedData, pubkey::Pubkey, signature::Signature,
};

use crate::errors::AccountsResult;

pub trait InternalAccountProvider {
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData>;
}

#[async_trait]
pub trait AccountCloner {
    async fn clone_account(
        &self,
        pubkey: &Pubkey,
        overrides: Option<AccountModification>,
    ) -> AccountsResult<Signature>;
}

#[async_trait]
pub trait AccountCommitter {
    /// Commits the account unless it determines that it isn't necessary, i.e. when the
    /// previously committed state is the same as the [commit_state_data].
    /// Returns the signature of the commit transaction if it was committed,
    /// otherwise [None].
    async fn commit_account(
        &self,
        delegated_account: Pubkey,
        commit_state_data: AccountSharedData,
    ) -> AccountsResult<Option<Signature>>;
}
