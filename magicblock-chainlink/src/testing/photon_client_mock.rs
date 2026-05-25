use std::{collections::HashMap, sync::Arc};

use magicblock_core::{compression::derive_cda_from_pda, Slot};
use parking_lot::Mutex;
use solana_account::Account;
use solana_pubkey::Pubkey;
use tonic::async_trait;

use crate::remote_account_provider::photon_client::{
    PhotonClient, PhotonClientResult,
};

#[derive(Clone, Default)]
pub struct PhotonClientMock {
    accounts: Arc<Mutex<HashMap<Pubkey, (Account, Slot)>>>,
}

impl PhotonClientMock {
    pub fn add_account(&self, pubkey: Pubkey, account: Account, slot: Slot) {
        let cda = derive_cda_from_pda(&pubkey);
        self.accounts.lock().insert(cda, (account, slot));
    }

    pub fn remove_account(&self, pubkey: &Pubkey) {
        let cda = derive_cda_from_pda(pubkey);
        self.accounts.lock().remove(&cda);
    }
}

#[async_trait]
impl PhotonClient for PhotonClientMock {
    async fn get_account(
        &self,
        pubkey: &Pubkey,
        min_context_slot: Option<Slot>,
    ) -> PhotonClientResult<Option<(Account, Slot)>> {
        let cda = derive_cda_from_pda(pubkey);
        if let Some((account, slot)) = self.accounts.lock().get(&cda) {
            if let Some(min_slot) = min_context_slot {
                if *slot < min_slot {
                    return Ok(None);
                }
            }
            return Ok(Some((account.clone(), *slot)));
        }
        Ok(None)
    }

    async fn get_multiple_accounts(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: Option<Slot>,
    ) -> PhotonClientResult<(Vec<Option<Account>>, Slot)> {
        let mut accs = Vec::with_capacity(pubkeys.len());
        // Seed with min_context_slot if present to better approximate the
        // "context slot" semantics of the real Photon client.
        let mut slot = min_context_slot.unwrap_or(0);
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
