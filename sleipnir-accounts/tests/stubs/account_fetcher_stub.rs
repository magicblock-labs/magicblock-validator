use std::collections::HashMap;

use async_trait::async_trait;
use conjunto_transwise::{
    account_fetcher::AccountFetcher, errors::TranswiseResult,
    transaction_accounts_holder::TransactionAccountsHolder,
    transaction_accounts_snapshot::TransactionAccountsSnapshot,
    AccountChainSnapshot, AccountChainSnapshotShared, AccountChainState,
    DelegationRecord,
};
use solana_sdk::{account::Account, clock::Slot, pubkey::Pubkey};

#[derive(Debug, Default)]
pub struct AccountFetcherStub {
    unknown_at_slot: Slot,
    known_accounts: HashMap<Pubkey, (Pubkey, Slot, Option<DelegationRecord>)>,
}

#[allow(unused)] // used in tests
impl AccountFetcherStub {
    pub fn add_undelegated(&mut self, pubkey: Pubkey, at_slot: Slot) {
        self.known_accounts
            .insert(pubkey, (Pubkey::new_unique(), at_slot, None));
    }
    pub fn add_delegated(
        &mut self,
        pubkey: Pubkey,
        at_slot: Slot,
        delegation_record: &DelegationRecord,
    ) {
        self.known_accounts.insert(
            pubkey,
            (
                Pubkey::new_unique(),
                at_slot,
                Some(delegation_record.clone()),
            ),
        );
    }
}

impl AccountFetcherStub {
    fn fetch_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> AccountChainSnapshotShared {
        let known_account = self.known_accounts.get(pubkey);
        match known_account {
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
    async fn fetch_transaction_accounts_snapshot(
        &self,
        accounts_holder: &TransactionAccountsHolder,
    ) -> TranswiseResult<TransactionAccountsSnapshot> {
        Ok(TransactionAccountsSnapshot {
            readonly: accounts_holder
                .readonly
                .iter()
                .map(|pubkey| self.fetch_account_chain_snapshot(pubkey))
                .collect(),
            writable: accounts_holder
                .writable
                .iter()
                .map(|pubkey| self.fetch_account_chain_snapshot(pubkey))
                .collect(),
            payer: accounts_holder.payer,
        })
    }
}
