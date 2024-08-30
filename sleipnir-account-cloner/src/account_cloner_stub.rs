use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use futures_util::future::{ready, BoxFuture};
use sleipnir_account_fetcher::AccountFetcherError;
use solana_sdk::pubkey::Pubkey;

use crate::{
    AccountCloner, AccountClonerError, AccountClonerOutput, AccountClonerResult,
};

#[derive(Debug, Clone, Default)]
pub struct AccountClonerStub {
    clone_account_outputs: Arc<RwLock<HashMap<Pubkey, AccountClonerOutput>>>,
}

impl AccountClonerStub {
    pub fn add(&self, pubkey: &Pubkey, output: AccountClonerOutput) {
        self.clone_account_outputs
            .write()
            .expect(
                "RwLock of AccountClonerStub.clone_account_outputs poisoned",
            )
            .insert(*pubkey, output);
    }
}

impl AccountCloner for AccountClonerStub {
    fn clone_account(
        &self,
        pubkey: &Pubkey,
    ) -> BoxFuture<AccountClonerResult<AccountClonerOutput>> {
        let output = self
            .clone_account_outputs
            .read()
            .expect(
                "RwLock of AccountClonerStub.clone_account_outputs poisoned",
            )
            .get(pubkey)
            .cloned()
            .ok_or(AccountClonerError::AccountFetcherError(
                AccountFetcherError::FailedToFetch(
                    "not included in stub".to_owned(),
                ),
            ));
        Box::pin(ready(output))
    }
}
