use std::collections::HashMap;

use async_trait::async_trait;
use conjunto_transwise::{
    AccountChainSnapshot, AccountChainSnapshotShared, AccountChainState,
    CommitFrequency, DelegationRecord,
};
use futures_util::future::{ready, BoxFuture};
use solana_sdk::{account::Account, clock::Slot, pubkey::Pubkey};

use crate::{AccountFetcher, AccountFetcherResult};

#[derive(Debug, Default)]
pub struct AccountFetcherStub {
    unknown_at_slot: Slot,
    known_accounts: HashMap<Pubkey, (Pubkey, Slot, Option<DelegationRecord>)>,
}

impl AccountFetcherStub {
    pub fn add_undelegated(&mut self, pubkey: Pubkey, at_slot: Slot) {
        self.known_accounts
            .insert(pubkey, (Pubkey::new_unique(), at_slot, None));
    }
    pub fn add_delegated(
        &mut self,
        pubkey: Pubkey,
        owner: Pubkey,
        at_slot: Slot,
    ) {
        self.known_accounts.insert(
            pubkey,
            (
                Pubkey::new_unique(),
                at_slot,
                Some(DelegationRecord {
                    owner,
                    commit_frequency: CommitFrequency::default(),
                }),
            ),
        );
    }
}

impl AccountFetcherStub {
    fn generate_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> AccountChainSnapshot {
        match self.known_accounts.get(pubkey) {
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
