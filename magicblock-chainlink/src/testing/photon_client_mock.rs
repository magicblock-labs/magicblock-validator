use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use magicblock_core::compression::derive_cda_from_pda;
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_sdk::clock::Slot;
use tokio::sync::RwLock;

use crate::{
    remote_account_provider::{
        photon_client::PhotonClient, RemoteAccountProviderResult,
    },
    testing::rpc_client_mock::AccountAtSlot,
};

#[derive(Clone, Default)]
pub struct PhotonClientMock {
    accounts: Arc<RwLock<HashMap<Pubkey, AccountAtSlot>>>,
}

impl PhotonClientMock {
    pub fn new() -> Self {
        Self {
            accounts: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn add_account(
        &self,
        pubkey: Pubkey,
        account: Account,
        slot: Slot,
    ) {
        let cda = derive_cda_from_pda(&pubkey);
        let mut accounts = self.accounts.write().await;
        accounts.insert(cda, AccountAtSlot { account, slot });
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
        if let Some(account_at_slot) = self.accounts.read().await.get(&cda) {
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
