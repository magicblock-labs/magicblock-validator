use futures_util::{
    future::{ready, BoxFuture},
    FutureExt,
};
use solana_sdk::pubkey::Pubkey;
use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, RwLock},
};
use tokio::sync::{
    mpsc::UnboundedSender,
    oneshot::{channel, Sender},
};

use crate::{
    AccountFetcher, AccountFetcherResult, RemoteAccountFetcherRequest,
    RemoteAccountFetcherWorker,
};

pub struct RemoteAccountFetcherClient {
    request_sender: UnboundedSender<RemoteAccountFetcherRequest>,
    snapshot_listeners:
        Arc<RwLock<HashMap<Pubkey, Vec<Sender<AccountFetcherResult>>>>>,
    snapshot_results: Arc<RwLock<HashMap<Pubkey, AccountFetcherResult>>>,
}

impl RemoteAccountFetcherClient {
    pub fn new(runner: &RemoteAccountFetcherWorker) -> Self {
        Self {
            request_sender: runner.get_request_sender(),
            snapshot_listeners: runner.get_snapshot_listeners(),
            snapshot_results: runner.get_snapshot_results(),
        }
    }
}

impl AccountFetcher for RemoteAccountFetcherClient {
    fn fetch_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> BoxFuture<AccountFetcherResult> {
        let (needs_sending, receiver) = match self
            .snapshot_listeners
            .write()
            .expect("RwLock of RemoteAccountFetcherClient.snapshot_listeners is poisoned")
            .entry(*pubkey)
        {
            Entry::Vacant(entry) => {
                let (sender, receiver) = channel();
                entry.insert(vec![sender]);
                (true, receiver)
            }
            Entry::Occupied(mut entry) => {
                let (sender, receiver) = channel();
                entry.get_mut().push(sender);
                (false, receiver)
            }
        };
        if needs_sending {
            if let Err(error) = self
                .request_sender
                .send(RemoteAccountFetcherRequest { account: *pubkey })
            {
                return Box::pin(ready(Err(error.to_string())));
            }
        }
        Box::pin(receiver.map(|received| match received {
            Ok(result) => result,
            Err(error) => Err(error.to_string()),
        }))
    }

    fn get_last_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountFetcherResult> {
        self.snapshot_results
            .read()
            .expect("RwLock of RemoteAccountFetcherClient.snapshot_results is poisoned")
            .get(pubkey)
            .cloned()
    }
}
