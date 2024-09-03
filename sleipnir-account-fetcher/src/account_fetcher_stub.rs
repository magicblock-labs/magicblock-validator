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
use solana_sdk::{account::Account, clock::Slot, pubkey::Pubkey};

use crate::{AccountFetcher, AccountFetcherResult};

#[derive(Debug, Clone, Default)]
pub struct AccountFetcherStub {
    unknown_at_slot: Slot,
    known_accounts:
        Arc<RwLock<HashMap<Pubkey, (Pubkey, Slot, Option<DelegationRecord>)>>>,
}

impl AccountFetcherStub {
    fn insert_known_account(
        &self,
        pubkey: Pubkey,
        owner: Pubkey,
        slot: Slot,
        delegation: Option<DelegationRecord>,
    ) {
        self.known_accounts
            .write()
            .unwrap()
            .insert(pubkey, (owner, slot, delegation));
    }
    fn generate_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> AccountChainSnapshot {
        match self.known_accounts.read().unwrap().get(pubkey) {
            Some((owner, at_slot, delegation_record)) => AccountChainSnapshot {
                pubkey: *pubkey,
                at_slot: *at_slot,
                chain_state: match delegation_record {
                    Some(delegation_record) => AccountChainState::Delegated {
                        account: Account {
                            owner: *owner,
                            ..Default::default()
                        },
                        delegation_pda: Pubkey::new_unique(),
                        delegation_record: delegation_record.clone(),
                    },
                    None => AccountChainState::Undelegated {
                        account: Account {
                            owner: *owner,
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
        .into()
    }
}

impl AccountFetcherStub {
    pub fn add_undelegated(&self, pubkey: Pubkey, at_slot: Slot) {
        self.insert_known_account(pubkey, Pubkey::new_unique(), at_slot, None);
    }
    pub fn add_delegated(
        &mut self,
        pubkey: Pubkey,
        owner: Pubkey,
        at_slot: Slot,
    ) {
        self.insert_known_account(
            pubkey,
            Pubkey::new_unique(),
            at_slot,
            Some(DelegationRecord {
                owner,
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
