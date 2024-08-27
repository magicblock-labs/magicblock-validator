use std::{
    collections::HashMap,
    sync::{RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use conjunto_transwise::AccountChainSnapshotShared;
use sleipnir_account_fetcher::AccountFetcher;
use sleipnir_account_updates::AccountUpdates;
use solana_sdk::{clock::Slot, pubkey::Pubkey};

use crate::errors::{AccountsError, AccountsResult};

#[derive(Debug, Default)]
pub struct ExternalAccountsCache<AFE, AUP>
where
    AFE: AccountFetcher,
    AUP: AccountUpdates,
{
    account_fetcher: AFE,
    account_updates: AUP,
    snapshots: RwLock<HashMap<Pubkey, AccountChainSnapshotShared>>,
}

impl<AFE, AUP> ExternalAccountsCache<AFE, AUP>
where
    AFE: AccountFetcher,
    AUP: AccountUpdates,
{
    pub async fn get_or_fetch(
        &self,
        pubkey: &Pubkey,
    ) -> AccountsResult<AccountChainSnapshotShared> {
        match self.snapshots_read_lock().get(pubkey) {
            Some(snapshot) => {
                if let Some(last_known_update_slot) =
                    self.account_updates.get_last_known_update_slot(pubkey)
                {
                    if last_known_update_slot > snapshot.at_slot {
                        return self.invalidate_and_fetch(pubkey).await;
                    }
                }
                Ok(snapshot.clone())
            }
            None => self.invalidate_and_fetch(pubkey).await,
        }
    }

    async fn invalidate_and_fetch(
        &self,
        pubkey: &Pubkey,
    ) -> AccountsResult<AccountChainSnapshotShared> {
        self.snapshots_write_lock().remove(pubkey);
        let fetched = self
            .account_fetcher
            .fetch_account_chain_snapshot(pubkey)
            .await?;
        self.snapshots_write_lock().insert(*pubkey, fetched.clone());
        Ok(fetched)
    }
}

impl<AFE, AUP> ExternalAccountsCache<AFE, AUP>
where
    AFE: AccountFetcher,
    AUP: AccountUpdates,
{
    fn snapshots_read_lock(
        &self,
    ) -> RwLockReadGuard<HashMap<Pubkey, AccountChainSnapshotShared>> {
        self.snapshots
            .read()
            .expect("RwLock of ExternalAccountsCache.snapshots is poisoned")
    }
    fn snapshots_write_lock(
        &self,
    ) -> RwLockWriteGuard<HashMap<Pubkey, AccountChainSnapshotShared>> {
        self.snapshots
            .write()
            .expect("RwLock of ExternalAccountsCache.snapshots is poisoned")
    }
}
