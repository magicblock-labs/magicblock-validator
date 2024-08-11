use std::{
    collections::{hash_map::Entry, HashMap},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, RwLock},
};

use conjunto_transwise::{
    account_fetcher::AccountFetcher, AccountChainSnapshotShared,
};
use futures_util::future::ready;
use log::warn;
use solana_sdk::pubkey::Pubkey;
use tokio::sync::oneshot::{channel, Receiver, Sender};

use crate::errors::{AccountsError, AccountsResult};

type CachedAccountFetcherError = String; // This error needs to be clonable to many receivers
type CachedAccountFetcherResult =
    Result<AccountChainSnapshotShared, CachedAccountFetcherError>;

#[derive(Debug)]
enum CachedAccountFetcherQuery {
    Fetching {
        listeners: Vec<Sender<CachedAccountFetcherResult>>,
    },
    Fetched {
        result: CachedAccountFetcherResult,
    },
}

#[derive(Debug)]
pub struct CachedAccountFetcher<AFE>
where
    AFE: AccountFetcher,
{
    account_fetcher: AFE,
    query_cache: Arc<Mutex<HashMap<Pubkey, CachedAccountFetcherQuery>>>,
}

impl<AFE> CachedAccountFetcher<AFE>
where
    AFE: AccountFetcher,
{
    pub fn new(account_fetcher: AFE) -> Self {
        Self {
            account_fetcher: Arc::new(Mutex::new(account_fetcher)),
            query_cache: Default::default(),
        }
    }

    pub async fn get_or_fetch_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> AccountsResult<AccountChainSnapshotShared> {
        // Lock the query_cache to get a resolvable future for the fetch result
        let future: Pin<
            Box<
                dyn Future<Output = AccountsResult<AccountChainSnapshotShared>>,
            >,
        > = match self
            .query_cache
            .lock()
            .expect("Mutex of CachedAccountFetcher.query_cache(1) is poisoned")
            .entry(*pubkey)
        {
            // If someone else already requested this account before, we'll rely on their fetch result
            Entry::Occupied(mut entry) => match entry.get_mut() {
                // If we have a in-flight request, just add ourselves as listener and wait for signal
                CachedAccountFetcherQuery::Fetching { listeners } => {
                    let (sender, receiver) = channel();
                    listeners.push(sender);
                    Box::pin(self.do_wait(receiver))
                }
                // If we already have fetched that account, we can finish immediately
                CachedAccountFetcherQuery::Fetched { result } => {
                    Box::pin(ready(
                        result
                            .clone()
                            .map_err(AccountsError::FailedToFetchAccount),
                    ))
                }
            },
            // If nobody requested this account before, create the cache item and start fetching asynchronously
            Entry::Vacant(entry) => {
                let (sender, receiver) = channel();
                entry.insert(CachedAccountFetcherQuery::Fetching {
                    listeners: vec![sender],
                });
                let pubkey = *pubkey;
                let account_fetcher = self.account_fetcher.clone();
                let query_cache = self.query_cache.clone();
                tokio::spawn(async move {
                    CachedAccountFetcher::do_fetch(
                        pubkey,
                        account_fetcher,
                        query_cache,
                    )
                    .await;
                });
                Box::pin(self.do_wait(receiver))
            }
        };
        // After we closed the write lock on the query_cache, we can await for the future
        future.await
    }

    async fn do_wait(
        &self,
        receiver: Receiver<CachedAccountFetcherResult>,
    ) -> AccountsResult<AccountChainSnapshotShared> {
        match receiver.await {
            Ok(result) => result.map_err(AccountsError::FailedToFetchAccount),
            Err(error) => {
                Err(AccountsError::FailedToFetchAccount(error.to_string()))
            }
        }
    }

    async fn do_fetch(
        pubkey: Pubkey,
        account_fetcher: Arc<Mutex<AFE>>,
        query_cache: Arc<Mutex<HashMap<Pubkey, CachedAccountFetcherQuery>>>,
    ) {
        // Schedule the fetch, and transform the result into a cloneable type
        let result = account_fetcher
            .lock()
            .expect("Mutex of CachedAccountFetcher.account_fetcher is poisoned")
            .fetch_account_chain_snapshot(&pubkey)
            .await
            .map_err(|error| error.to_string());
        // Lock the query_cache to update the result of the fetch and get the list of listeners
        let listeners = match query_cache
            .lock()
            .expect("Mutex of CachedAccountFetcher.query_cache(2) is poisoned")
            .entry(pubkey)
        {
            // If the entry didn't exist for some reason, something is very wront, just fail here
            Entry::Vacant(_) => {
                return warn!("Fetch receiver was discarded improperly",);
            }
            // If the entry exists, we want to override its content with the fetch result
            Entry::Occupied(mut entry) => {
                // First protect against weird case of the fetch having been already processed somehow
                if let CachedAccountFetcherQuery::Fetched { .. } = entry.get() {
                    return warn!("Fetch was already done",);
                }
                // Get the list of listeners bu replacing the content of the entry with the fetch result
                match entry.insert(CachedAccountFetcherQuery::Fetched {
                    result: result.clone(),
                }) {
                    CachedAccountFetcherQuery::Fetched { .. } => vec![], // This should never happen
                    CachedAccountFetcherQuery::Fetching { listeners } => {
                        listeners
                    }
                }
            }
        };
        // Now that we have the other listeners, lets notify them
        for listener in listeners {
            if let Err(error) = listener.send(result.clone()) {
                warn!("Could not send fetch result to listener: {:?}", error);
            }
        }
    }
}
