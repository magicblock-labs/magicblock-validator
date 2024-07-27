use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use sleipnir_accounts::{
    errors::AccountsResult, AccountCommittee, AccountCommitter,
    CommitAccountsPayload, SendableCommitAccountsPayload,
};
use solana_sdk::{
    account::AccountSharedData, hash::Hash, pubkey::Pubkey,
    signature::Signature, transaction::Transaction,
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
    async fn create_commit_accounts_transactions(
        &self,
        committees: Vec<AccountCommittee>,
        _latest_blockhash: Option<Hash>,
    ) -> AccountsResult<Vec<CommitAccountsPayload>> {
        let transaction = Transaction::default();
        let payload = CommitAccountsPayload {
            transaction: Some(transaction),
            committees: committees
                .iter()
                .map(|x| (x.pubkey, x.account_data.clone()))
                .collect(),
        };
        Ok(vec![payload])
    }

    async fn send_commit_transactions(
        &self,
        payloads: Vec<SendableCommitAccountsPayload>,
    ) -> AccountsResult<Vec<Signature>> {
        let signatures =
            payloads.iter().map(|_| Signature::new_unique()).collect();
        for payload in payloads {
            for (pubkey, account) in payload.committees {
                self.committed_accounts
                    .write()
                    .unwrap()
                    .insert(pubkey, account);
            }
        }
        Ok(signatures)
    }
}
