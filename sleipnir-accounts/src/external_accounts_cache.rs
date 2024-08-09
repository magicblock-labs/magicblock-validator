use std::{
    collections::{hash_map::Entry, HashMap},
    future::Future,
    pin::Pin,
    sync::{RwLock, RwLockWriteGuard},
};

use conjunto_transwise::{
    account_fetcher::AccountFetcher, AccountChainSnapshotShared,
};
use futures_util::future::ready;
use solana_sdk::pubkey::Pubkey;
use tokio::sync::oneshot::{channel, Receiver, Sender};

use crate::errors::{AccountsError, AccountsResult};

pub type ExternalAccountsCacheError = String; // This error needs to be clonable to many receivers
pub type ExternalAccountsCacheResult =
    Result<AccountChainSnapshotShared, ExternalAccountsCacheError>;

pub enum ExternalAccountsCacheItem {
    Fetching {
        listeners: Vec<Sender<ExternalAccountsCacheResult>>,
    },
    Fetched {
        result: ExternalAccountsCacheResult,
    },
}

pub struct ExternalAccountsCache {
    database: RwLock<HashMap<Pubkey, ExternalAccountsCacheItem>>,
}

impl ExternalAccountsCache {
    pub fn new() -> Self {
        Self {
            database: Default::default(),
        }
    }

    pub async fn get_or_fetch<AFE: AccountFetcher>(
        &self,
        pubkey: &Pubkey,
        account_fetcher: &AFE,
    ) -> AccountsResult<AccountChainSnapshotShared> {
        // Lock the database to get a resolvable future for the fetch result (but dont actually do any fetch here)
        let future: Pin<
            Box<
                dyn Future<Output = AccountsResult<AccountChainSnapshotShared>>,
            >,
        > = match self.database_lock().entry(*pubkey) {
            // If someone else already requested this account before, we'll rely on their fetch result
            Entry::Occupied(mut entry) => match entry.get_mut() {
                // If we have a in-flight request, just add ourselves as listener and wait for signal
                ExternalAccountsCacheItem::Fetching { listeners } => {
                    let (sender, receiver) = channel();
                    listeners.push(sender);
                    Box::pin(self.do_wait(receiver))
                }
                // If we already have fetched that account, we can finish immediately
                ExternalAccountsCacheItem::Fetched { result } => {
                    Box::pin(ready(
                        result
                            .clone()
                            .map_err(AccountsError::FailedToFetchAccount),
                    ))
                }
            },
            // If nobody requested this account before, create the cache item and start fetching
            Entry::Vacant(entry) => {
                entry.insert(ExternalAccountsCacheItem::Fetching {
                    listeners: vec![],
                });
                Box::pin(self.do_fetch(pubkey, account_fetcher))
            }
        };
        // After we closed the write lock on the database, we can await for the future
        future.await
    }

    async fn do_fetch<AFE: AccountFetcher>(
        &self,
        pubkey: &Pubkey,
        account_fetcher: &AFE,
    ) -> AccountsResult<AccountChainSnapshotShared> {
        // Schedule the fetch, and transform the result into a cloneable type
        let result = account_fetcher
            .fetch_account_chain_snapshot(pubkey)
            .await
            .map_err(|error| error.to_string());
        // Lock the database to update the result of the fetch and get the list of listeners
        let listeners = match self.database_lock().entry(*pubkey) {
            // If the entry didn't exist for some reason, something is very wront, just fail here
            Entry::Vacant(_) => {
                return Err(AccountsError::FailedToResolveAccount(
                    "Fetch receiver was discarded improperly".to_string(),
                ));
            }
            // If the entry exists, we want to override its content with the fetch result
            Entry::Occupied(mut entry) => {
                // First protect against weird case of the fetch having been already processed somehow
                match entry.get() {
                    ExternalAccountsCacheItem::Fetched { .. } => {
                        return Err(AccountsError::FailedToResolveAccount(
                            "Fetch was already done".to_string(),
                        ));
                    }
                    ExternalAccountsCacheItem::Fetching { .. } => {}
                };
                // Get the list of listeners bu replacing the content of the entry with the fetch result
                match entry.insert(ExternalAccountsCacheItem::Fetched {
                    result: result.clone(),
                }) {
                    ExternalAccountsCacheItem::Fetched { .. } => vec![], // This should never happen
                    ExternalAccountsCacheItem::Fetching { listeners } => {
                        listeners
                    }
                }
            }
        };
        // Now that we have the other listeners, lets notify them
        for listener in listeners {
            listener.send(result.clone());
        }
        // Done
        result.map_err(AccountsError::FailedToFetchAccount)
    }

    async fn do_wait(
        &self,
        receiver: Receiver<ExternalAccountsCacheResult>,
    ) -> AccountsResult<AccountChainSnapshotShared> {
        match receiver.await {
            Ok(result) => result.map_err(AccountsError::FailedToFetchAccount),
            Err(error) => {
                Err(AccountsError::FailedToResolveAccount(error.to_string()))
            }
        }
    }

    fn database_lock(
        &self,
    ) -> RwLockWriteGuard<HashMap<Pubkey, ExternalAccountsCacheItem>> {
        self.database
            .write()
            .expect("RwLock of ExternalAccountsCache.database is poisoned")
    }
}
