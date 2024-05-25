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
    async fn commit_account(
        &self,
        delegated_account: Pubkey,
        committed_state_data: Vec<u8>,
    ) -> AccountsResult<Signature>;
}
