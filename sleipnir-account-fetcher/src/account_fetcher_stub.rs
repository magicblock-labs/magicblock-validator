use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use conjunto_transwise::{
    AccountChainSnapshot, AccountChainSnapshotShared, AccountChainState,
    CommitFrequency, DelegationRecord,
};
use futures_util::future::{ready, BoxFuture};
use solana_sdk::{
    account::Account, clock::Slot, pubkey::Pubkey, system_program,
};

use crate::{AccountFetcher, AccountFetcherResult};

#[derive(Debug)]
struct AccountFetcherStubKnownAccount {
    owner: Pubkey,
    slot: Slot,
    delegation_record: Option<DelegationRecord>,
}

#[derive(Debug, Clone, Default)]
pub struct AccountFetcherStub {
    unknown_at_slot: Slot,
    known_accounts:
        Arc<RwLock<HashMap<Pubkey, AccountFetcherStubKnownAccount>>>,
}

impl AccountFetcherStub {
    fn insert_known_account(
        &self,
        pubkey: Pubkey,
        owner: Pubkey,
        slot: Slot,
        delegation_record: Option<DelegationRecord>,
    ) {
        self.known_accounts.write().unwrap().insert(
            pubkey,
            AccountFetcherStubKnownAccount {
                owner,
                slot,
                delegation_record,
            },
        );
    }
    fn generate_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> AccountChainSnapshot {
        match self.known_accounts.read().unwrap().get(pubkey) {
            Some(known_account) => AccountChainSnapshot {
                pubkey: *pubkey,
                at_slot: known_account.slot,
                chain_state: match &known_account.delegation_record {
                    Some(delegation_record) => AccountChainState::Delegated {
                        account: Account {
                            owner: known_account.owner,
                            ..Default::default()
                        },
                        delegation_pda: Pubkey::new_unique(),
                        delegation_record: delegation_record.clone(),
                    },
                    None => AccountChainState::Undelegated {
                        account: Account {
                            owner: known_account.owner,
                            ..Default::default()
                        },
                    },
                },
            },
            None => AccountChainSnapshot {
                pubkey: *pubkey,
                at_slot: self.unknown_at_slot,
                chain_state: AccountChainState::NewAccount,
            },
        }
    }
}

impl AccountFetcherStub {
    pub fn set_system_account(&self, pubkey: Pubkey, at_slot: Slot) {
        self.insert_known_account(pubkey, system_program::ID, at_slot, None);
    }
    pub fn set_undelegated(&self, pubkey: Pubkey, at_slot: Slot) {
        self.insert_known_account(pubkey, Pubkey::new_unique(), at_slot, None);
    }
    pub fn set_delegated(&self, pubkey: Pubkey, owner: Pubkey, at_slot: Slot) {
        self.insert_known_account(
            pubkey,
            Pubkey::new_unique(),
            at_slot,
            Some(DelegationRecord {
                owner,
                delegation_slot: 0,
                commit_frequency: CommitFrequency::default(),
            }),
        );
    }
}

#[async_trait]
impl AccountFetcher for AccountFetcherStub {
    fn fetch_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> BoxFuture<AccountFetcherResult<AccountChainSnapshotShared>> {
        Box::pin(ready(Ok(self
            .generate_account_chain_snapshot(pubkey)
            .into())))
    }
}
