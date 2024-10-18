use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use sleipnir_accounts::{
    errors::AccountsResult, AccountCommittee, AccountCommitter,
    CommitAccountsPayload, CommitAccountsTransaction, PendingCommitTransaction,
    SendableCommitAccountsPayload,
};
use sleipnir_metrics::metrics;
use solana_sdk::{
    account::AccountSharedData, pubkey::Pubkey, signature::Signature,
    transaction::Transaction,
};

#[derive(Debug, Default, Clone)]
pub struct AccountCommitterStub {
    committed_accounts: Arc<RwLock<HashMap<Pubkey, AccountSharedData>>>,
    comfirmed_transactions: Arc<RwLock<HashSet<Signature>>>,
}

#[allow(unused)] // used in tests
impl AccountCommitterStub {
    pub fn len(&self) -> usize {
        self.committed_accounts.read().unwrap().len()
    }
    pub fn committed(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.committed_accounts.read().unwrap().get(pubkey).cloned()
    }
    pub fn confirmed(&self, signature: &Signature) -> bool {
        self.comfirmed_transactions
            .read()
            .unwrap()
            .contains(signature)
    }
}

#[async_trait]
impl AccountCommitter for AccountCommitterStub {
    async fn create_commit_accounts_transaction(
        &self,
        committees: Vec<AccountCommittee>,
    ) -> AccountsResult<CommitAccountsPayload> {
        let transaction = Transaction::default();
        let payload = CommitAccountsPayload {
            transaction: Some(CommitAccountsTransaction {
                transaction,
                undelegated_accounts: HashSet::new(),
                committed_only_accounts: HashSet::new(),
            }),
            committees: committees
                .iter()
                .map(|x| (x.pubkey, x.account_data.clone()))
                .collect(),
        };
        Ok(payload)
    }

    async fn send_commit_transactions(
        &self,
        payloads: Vec<SendableCommitAccountsPayload>,
    ) -> AccountsResult<Vec<PendingCommitTransaction>> {
        let signatures = payloads
            .iter()
            .map(|_| PendingCommitTransaction {
                signature: Signature::new_unique(),
                undelegated_accounts: HashSet::new(),
                committed_only_accounts: HashSet::new(),
                timer: metrics::account_commit_start(),
            })
            .collect();
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

    async fn confirm_pending_commits(
        &self,
        pending_commits: Vec<PendingCommitTransaction>,
    ) {
        for commit in pending_commits {
            self.comfirmed_transactions
                .write()
                .unwrap()
                .insert(commit.signature);
        }
    }
}
