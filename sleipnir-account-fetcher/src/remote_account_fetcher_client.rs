use async_trait::async_trait;
use std::{
    collections::{hash_map::Entry, HashMap},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use futures_util::{future::ready, FutureExt};
use log::*;
use solana_sdk::pubkey::{self, Pubkey};
use tokio::sync::{
    mpsc::UnboundedSender,
    oneshot::{channel, Receiver},
};

use crate::{
    AccountFetcher, RemoteAccountFetcherResponse, RemoteAccountFetcherResult,
    RemoteAccountFetcherRunner,
};

pub struct RemoteAccountFetcherClient {
    request_sender: UnboundedSender<Pubkey>,
    responses: Arc<Mutex<HashMap<Pubkey, RemoteAccountFetcherResponse>>>,
}

impl RemoteAccountFetcherClient {
    pub fn new(runner: &RemoteAccountFetcherRunner) -> Self {
        Self {
            request_sender: runner.get_request_sender(),
            responses: runner.get_responses(),
        }
    }
}

impl RemoteAccountFetcherClient {
    fn get_or_request_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> Pin<Box<dyn Future<Output = RemoteAccountFetcherResult>>> {
        match self
            .responses
            .lock()
            .expect("Mutex of RemoteAccountFetcherClient.responses is poisoned")
            .entry(*pubkey)
        {
            // If someone else already requested this account before, we'll rely on their fetch result
            Entry::Occupied(mut entry) => match entry.get_mut() {
                // If we have a in-flight request, just add ourselves as listener and wait for signal
                RemoteAccountFetcherResponse::InFlight { listeners } => {
                    let (sender, receiver) = channel();
                    listeners.push(sender);
                    Box::pin(RemoteAccountFetcherClient::wait_recv(receiver))
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
                Box::pin(RemoteAccountFetcherClient::wait_recv(receiver))
            }
        }
    }

    async fn wait_recv(
        receiver: Receiver<RemoteAccountFetcherResult>,
    ) -> RemoteAccountFetcherResult {
        match receiver.await {
            Ok(result) => result,
            Err(error) => Err(error.to_string()),
        }
    }
}

#[async_trait]
impl AccountFetcher for RemoteAccountFetcherClient {
    async fn get_or_fetch_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> RemoteAccountFetcherResult {
        self.get_or_request_account_chain_snapshot(pubkey).await
    }
}
