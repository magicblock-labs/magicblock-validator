use std::{
    collections::{hash_map::Entry, HashMap},
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
enum AccountFetcherStubState {
    Basic { owner: Pubkey },
    Delegated { delegation_record: DelegationRecord },
    Executable,
}

#[derive(Debug)]
struct AccountFetcherStubSnapshot {
    slot: Slot,
    state: AccountFetcherStubState,
}

#[derive(Debug, Clone, Default)]
pub struct AccountFetcherStub {
    unknown_at_slot: Slot,
    fetched_counters: Arc<RwLock<HashMap<Pubkey, u64>>>,
    known_accounts: Arc<RwLock<HashMap<Pubkey, AccountFetcherStubSnapshot>>>,
}

impl AccountFetcherStub {
    fn insert_known_account(
        &self,
        pubkey: Pubkey,
        info: AccountFetcherStubSnapshot,
    ) {
        self.known_accounts.write().unwrap().insert(pubkey, info);
    }
    fn generate_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> AccountChainSnapshot {
        match self.known_accounts.read().unwrap().get(pubkey) {
            Some(known_account) => AccountChainSnapshot {
                pubkey: *pubkey,
                at_slot: known_account.slot,
                chain_state: match &known_account.state {
                    AccountFetcherStubState::Basic { owner } => {
                        AccountChainState::Undelegated {
                            account: Account {
                                owner: *owner,
                                ..Default::default()
                            },
                        }
                    }
                    AccountFetcherStubState::Executable => {
                        AccountChainState::Undelegated {
                            account: Account {
                                executable: true,
                                ..Default::default()
                            },
                        }
                    }
                    AccountFetcherStubState::Delegated {
                        delegation_record,
                    } => AccountChainState::Delegated {
                        account: Default::default(),
                        delegation_pda: Pubkey::new_unique(),
                        delegation_record: delegation_record.clone(),
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
        self.insert_known_account(
            pubkey,
            AccountFetcherStubSnapshot {
                slot: at_slot,
                state: AccountFetcherStubState::Basic {
                    owner: system_program::ID,
                },
            },
        );
    }
    pub fn set_pda_account(&self, pubkey: Pubkey, at_slot: Slot) {
        self.insert_known_account(
            pubkey,
            AccountFetcherStubSnapshot {
                slot: at_slot,
                state: AccountFetcherStubState::Basic {
                    owner: Pubkey::new_unique(),
                },
            },
        );
    }
    pub fn set_delegated_account(
        &self,
        pubkey: Pubkey,
        owner: Pubkey,
        at_slot: Slot,
    ) {
        self.insert_known_account(
            pubkey,
            AccountFetcherStubSnapshot {
                slot: at_slot,
                state: AccountFetcherStubState::Delegated {
                    delegation_record: DelegationRecord {
                        owner,
                        delegation_slot: 0,
                        commit_frequency: CommitFrequency::default(),
                    },
                },
            },
        );
    }
    pub fn set_executable_account(&self, pubkey: Pubkey, at_slot: Slot) {
        self.insert_known_account(
            pubkey,
            AccountFetcherStubSnapshot {
                slot: at_slot,
                state: AccountFetcherStubState::Executable,
            },
        );
    }

    pub fn get_fetch_count(&self, pubkey: &Pubkey) -> u64 {
        self.fetched_counters
            .read()
            .unwrap()
            .get(pubkey)
            .cloned()
            .unwrap_or(0)
    }
}

#[async_trait]
impl AccountFetcher for AccountFetcherStub {
    fn fetch_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> BoxFuture<AccountFetcherResult<AccountChainSnapshotShared>> {
        match self.fetched_counters.write().unwrap().entry(*pubkey) {
            Entry::Occupied(mut entry) => {
                *entry.get_mut() = *entry.get() + 1;
            }
            Entry::Vacant(entry) => {
                entry.insert(1);
            }
        };
        Box::pin(ready(Ok(self
            .generate_account_chain_snapshot(pubkey)
            .into())))
    }
}
