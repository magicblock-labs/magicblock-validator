use async_trait::async_trait;
use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, Mutex},
};

use futures_util::{
    future::{ready, BoxFuture},
    FutureExt,
};
use solana_sdk::pubkey::Pubkey;
use tokio::sync::{mpsc::UnboundedSender, oneshot::channel};

use crate::{
    AccountFetcher, AccountFetcherResult, RemoteAccountFetcherRequest,
    RemoteAccountFetcherResponse, RemoteAccountFetcherWorker,
};

pub struct RemoteAccountFetcherClient {
    request_sender: UnboundedSender<RemoteAccountFetcherRequest>,
    snapshot_responses:
        Arc<Mutex<HashMap<Pubkey, RemoteAccountFetcherResponse>>>,
}

impl RemoteAccountFetcherClient {
    pub fn new(runner: &RemoteAccountFetcherWorker) -> Self {
        Self {
            request_sender: runner.get_request_sender(),
            snapshot_responses: runner.get_snapshot_responses(),
        }
    }
}

#[async_trait]
impl AccountFetcher for RemoteAccountFetcherClient {
    async fn get_or_fetch_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> AccountFetcherResult {
        // First, we lock the response mutex and create the future that resolves the respons
        let future: BoxFuture<AccountFetcherResult> = match self
            .snapshot_responses
            .lock()
            .expect("Mutex of RemoteAccountFetcherClient.snapshot_responses is poisoned")
            .entry(*pubkey)
        {
            // If someone else already requested this account before, we'll rely on their fetch result
            Entry::Occupied(mut entry) => match entry.get_mut() {
                // If we have a in-flight request, just add ourselves as listener and wait for signal
                RemoteAccountFetcherResponse::InFlight { listeners } => {
                    let (sender, receiver) = channel();
                    listeners.push(sender);
                    Box::pin(receiver.map(|received| match received {
                        Ok(result) => result,
                        Err(error) => Err(error.to_string()),
                    }))
                }
                // If we already have fetched that account, we can finish immediately
                RemoteAccountFetcherResponse::Available { result } => {
                    Box::pin(ready(result.clone()))
                }
            },
            // If nobody requested this account before, create the cache item and start fetching asynchronously
            Entry::Vacant(entry) => {
                let (sender, receiver) = channel();
                entry.insert(RemoteAccountFetcherResponse::InFlight {
                    listeners: vec![sender],
                });
                self.request_sender
                    .send(RemoteAccountFetcherRequest { account: *pubkey })
                    .map_err(|error| error.to_string())?;
                Box::pin(receiver.map(|received| match received {
                    Ok(result) => result,
                    Err(error) => Err(error.to_string()),
                }))
            }
        };
        // Once the lock has been release, awaits the future for the response
        future.await
    }
}
