use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use conjunto_transwise::{
    AccountChainSnapshot, AccountChainSnapshotShared, AccountChainState,
    CommitFrequency, DelegationInconsistency, DelegationRecord,
};
use futures_util::future::{ready, BoxFuture};
use solana_sdk::{account::Account, clock::Slot, pubkey::Pubkey};

use crate::{AccountFetcher, AccountFetcherResult};

#[derive(Debug)]
enum AccountFetcherStubState {
    Wallet,
    Executable,
    Undelegated { owner: Pubkey },
    Delegated { delegation_record: DelegationRecord },
}

#[derive(Debug)]
struct AccountFetcherStubSnapshot {
    slot: Slot,
    state: AccountFetcherStubState,
}

#[derive(Debug, Clone, Default)]
pub struct AccountFetcherStub {
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
    ) -> AccountFetcherResult<AccountChainSnapshotShared> {
        match self.known_accounts.read().unwrap().get(pubkey) {
            Some(known_account) => Ok(AccountChainSnapshot {
                pubkey: *pubkey,
                at_slot: known_account.slot,
                chain_state: match &known_account.state {
                    AccountFetcherStubState::Wallet => {
                        AccountChainState::Wallet {
                            lamports: 42,
                            owner: Pubkey::new_unique(),
                        }
                    }
                    AccountFetcherStubState::Undelegated { owner } => {
                        AccountChainState::Undelegated {
                            account: Account {
                                owner: *owner,
                                ..Default::default()
                            },
                            delegation_inconsistency: DelegationInconsistency::DelegationRecordNotFound,
                        }
                    }
                    AccountFetcherStubState::Executable => {
                        AccountChainState::Undelegated {
                            account: Account {
                                executable: true,
                                ..Default::default()
                            },
                            delegation_inconsistency: DelegationInconsistency::DelegationRecordNotFound,
                        }
                    }
                    AccountFetcherStubState::Delegated {
                        delegation_record,
                    } => AccountChainState::Delegated {
                        account: Default::default(),
                        delegation_record: delegation_record.clone(),
                    },
                },
            }
            .into()),
            None => Err(crate::AccountFetcherError::FailedToFetch(format!(
                "Account not supposed to be fetched during the tests: {:?}",
                pubkey
            ))),
        }
    }
}

impl AccountFetcherStub {
    pub fn set_wallet_account(&self, pubkey: Pubkey, at_slot: Slot) {
        self.insert_known_account(
            pubkey,
            AccountFetcherStubSnapshot {
                slot: at_slot,
                state: AccountFetcherStubState::Wallet,
            },
        );
    }
    pub fn set_undelegated_account(&self, pubkey: Pubkey, at_slot: Slot) {
        self.insert_known_account(
            pubkey,
            AccountFetcherStubSnapshot {
                slot: at_slot,
                state: AccountFetcherStubState::Undelegated {
                    owner: Pubkey::new_unique(),
                },
            },
        );
    }
    pub fn set_delegated_account(
        &self,
        pubkey: Pubkey,
        at_slot: Slot,
        delegation_slot: Slot,
    ) {
        self.insert_known_account(
            pubkey,
            AccountFetcherStubSnapshot {
                slot: at_slot,
                state: AccountFetcherStubState::Delegated {
                    delegation_record: DelegationRecord {
                        authority: Pubkey::new_unique(),
                        owner: Pubkey::new_unique(),
                        delegation_slot,
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
        Box::pin(ready(self.generate_account_chain_snapshot(pubkey)))
    }
}
