use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, RwLock},
};

use futures_util::{
    future::{ready, BoxFuture},
    FutureExt,
};
use sleipnir_account_fetcher::AccountFetcher;
use sleipnir_account_updates::AccountUpdates;
use solana_sdk::pubkey::Pubkey;
use tokio::sync::{
    mpsc::UnboundedSender,
    oneshot::{channel, Sender},
};

use crate::{
    AccountCloner, AccountClonerError, AccountClonerResult,
    RemoteAccountClonerWorker,
};

pub struct RemoteAccountClonerClient {
    clone_request_sender: UnboundedSender<Pubkey>,
    clone_result_listeners:
        Arc<RwLock<HashMap<Pubkey, Vec<Sender<AccountClonerResult>>>>>,
}

impl RemoteAccountClonerClient {
    pub fn new<AFE, AUP>(worker: &RemoteAccountClonerWorker<AFE, AUP>) -> Self
    where
        AFE: AccountFetcher,
        AUP: AccountUpdates,
    {
        Self {
            clone_request_sender: worker.get_clone_request_sender(),
            clone_result_listeners: worker.get_clone_result_listeners(),
        }
    }
}

impl AccountCloner for RemoteAccountClonerClient {
    fn clone_account(&self, pubkey: &Pubkey) -> BoxFuture<AccountClonerResult> {
        let (should_request_clone, receiver) = match self
            .clone_result_listeners
            .write()
            .expect("RwLock of RemoteAccountClonerClient.clone_result_listeners is poisoned")
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
        if should_request_clone {
            if let Err(error) = self.clone_request_sender.send(*pubkey) {
                return Box::pin(ready(Err(AccountClonerError::SendError(
                    error,
                ))));
            }
        }
        Box::pin(receiver.map(|received| match received {
            Ok(result) => result,
            Err(error) => Err(AccountClonerError::RecvError(error)),
        }))
    }
}
