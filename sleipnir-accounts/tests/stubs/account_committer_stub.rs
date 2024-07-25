use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use sleipnir_accounts::{errors::AccountsResult, AccountCommitter};
use solana_sdk::{
    account::AccountSharedData, pubkey::Pubkey, signature::Signature,
    transaction::Transaction,
};

#[derive(Debug, Default, Clone)]
pub struct AccountCommitterStub {
    committed_accounts: Arc<RwLock<HashMap<Pubkey, AccountSharedData>>>,
}

#[allow(unused)] // used in tests
impl AccountCommitterStub {
    pub fn len(&self) -> usize {
        self.committed_accounts.read().unwrap().len()
    }
    pub fn committed(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.committed_accounts.read().unwrap().get(pubkey).cloned()
    }
}

#[async_trait]
impl AccountCommitter for AccountCommitterStub {
    async fn create_commit_account_transaction(
        &self,
        _delegated_account: Pubkey,
        _commit_state_data: AccountSharedData,
    ) -> AccountsResult<Option<Transaction>> {
        Ok(Some(Transaction::default()))
    }

    async fn commit_account(
        &self,
        delegated_account: Pubkey,
        commit_state_data: AccountSharedData,
        _transaction: Transaction,
    ) -> AccountsResult<Signature> {
        self.committed_accounts
            .write()
            .unwrap()
            .insert(delegated_account, commit_state_data);
        Ok(Signature::new_unique())
    }
}
