#![cfg(any(test, feature = "dev-context"))]
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use magicblock_core::compression::derive_cda_from_pda;
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_sdk::clock::Slot;

use crate::{
    remote_account_provider::{
        photon_client::PhotonClient, RemoteAccountProviderResult,
    },
    testing::rpc_client_mock::AccountAtSlot,
};

#[derive(Clone, Default)]
pub struct PhotonClientMock {
    accounts: Arc<Mutex<HashMap<Pubkey, AccountAtSlot>>>,
}

impl PhotonClientMock {
    pub fn new() -> Self {
        Self {
            accounts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn add_account(&self, pubkey: Pubkey, account: Account, slot: Slot) {
        let cda = derive_cda_from_pda(&pubkey);
        let mut accounts = self.accounts.lock().unwrap();
        accounts.insert(cda, AccountAtSlot { account, slot });
    }

    pub fn add_acounts(&self, new_accounts: HashMap<Pubkey, AccountAtSlot>) {
        let mut accounts = self.accounts.lock().unwrap();
        for (pubkey, account_at_slot) in new_accounts {
            let cda = derive_cda_from_pda(&pubkey);
            accounts.insert(cda, account_at_slot);
        }
    }
}

#[async_trait]
impl PhotonClient for PhotonClientMock {
    async fn get_account(
        &self,
        pubkey: &Pubkey,
        min_context_slot: Option<Slot>,
    ) -> RemoteAccountProviderResult<Option<(Account, Slot)>> {
        let cda = derive_cda_from_pda(pubkey);
        let accounts = self.accounts.lock().unwrap();
        if let Some(account_at_slot) = accounts.get(&cda) {
            if let Some(min_slot) = min_context_slot {
                if account_at_slot.slot < min_slot {
                    return Ok(None);
                }
            }
            return Ok(Some((
                account_at_slot.account.clone(),
                account_at_slot.slot,
            )));
        }
        Ok(None)
    }

    async fn get_multiple_accounts(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: Option<Slot>,
    ) -> RemoteAccountProviderResult<(Vec<Option<Account>>, Slot)> {
        let mut accs = vec![];
        let mut slot = 0;
        for pubkey in pubkeys {
            let account = self.get_account(pubkey, min_context_slot).await?;
            if let Some((ref _acc, acc_slot)) = account {
                if acc_slot > slot {
                    slot = acc_slot;
                }
            }
            accs.push(account.map(|(acc, _)| acc));
        }
        Ok((accs, slot))
    }
}
