use async_trait::async_trait;
use sleipnir_mutator::AccountModification;
use solana_sdk::account::AccountSharedData;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;

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
