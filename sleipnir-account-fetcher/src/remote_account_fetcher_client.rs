use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, RwLock},
};

use futures_util::{
    future::{ready, BoxFuture},
    FutureExt,
};
use solana_sdk::pubkey::Pubkey;
use tokio::sync::{
    mpsc::UnboundedSender,
    oneshot::{channel, Sender},
};

use crate::{
    AccountFetcher, AccountFetcherError, AccountFetcherResult,
    RemoteAccountFetcherWorker,
};

pub struct RemoteAccountFetcherClient {
    request_sender: UnboundedSender<Pubkey>,
    fetch_result_listeners:
        Arc<RwLock<HashMap<Pubkey, Vec<Sender<AccountFetcherResult>>>>>,
}

impl RemoteAccountFetcherClient {
    pub fn new(runner: &RemoteAccountFetcherWorker) -> Self {
        Self {
            request_sender: runner.get_request_sender(),
            fetch_result_listeners: runner.get_fetch_result_listeners(),
        }
    }
}

impl AccountFetcher for RemoteAccountFetcherClient {
    fn fetch_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> BoxFuture<AccountFetcherResult> {
        let (is_first_request_to_fetch, receiver) = match self
            .fetch_result_listeners
            .write()
            .expect("RwLock of RemoteAccountFetcherClient.fetch_result_listeners is poisoned")
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
        if is_first_request_to_fetch {
            if let Err(error) = self.request_sender.send(*pubkey) {
                return Box::pin(ready(Err(AccountFetcherError::SendError(
                    error,
                ))));
            }
        }
        Box::pin(receiver.map(|received| match received {
            Ok(result) => result,
            Err(error) => Err(AccountFetcherError::RecvError(error)),
        }))
    }
}
