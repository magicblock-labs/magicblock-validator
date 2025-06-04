use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, RwLock},
};

use futures_util::{
    future::{ready, BoxFuture},
    FutureExt,
};
use magicblock_account_dumper::AccountDumper;
use magicblock_account_fetcher::AccountFetcher;
use magicblock_account_updates::AccountUpdates;
use magicblock_accounts_api::InternalAccountProvider;
use magicblock_committor_service::ChangesetCommittor;
use solana_sdk::pubkey::Pubkey;
use tokio::sync::{mpsc::UnboundedSender, oneshot::channel};

use crate::{
    AccountCloner, AccountClonerError, AccountClonerListeners,
    AccountClonerOutput, AccountClonerResult, RemoteAccountClonerWorker,
};

pub struct RemoteAccountClonerClient {
    clone_request_sender: UnboundedSender<Pubkey>,
    clone_listeners: Arc<RwLock<HashMap<Pubkey, AccountClonerListeners>>>,
}

impl RemoteAccountClonerClient {
    pub fn new<IAP, AFE, AUP, ADU, CC>(
        worker: &RemoteAccountClonerWorker<IAP, AFE, AUP, ADU, CC>,
    ) -> Self
    where
        IAP: InternalAccountProvider,
        AFE: AccountFetcher,
        AUP: AccountUpdates,
        ADU: AccountDumper,
        CC: ChangesetCommittor,
    {
        Self {
            clone_request_sender: worker.get_clone_request_sender(),
            clone_listeners: worker.get_clone_listeners(),
        }
    }
}

impl AccountCloner for RemoteAccountClonerClient {
    fn clone_account(
        &self,
        pubkey: &Pubkey,
    ) -> BoxFuture<AccountClonerResult<AccountClonerOutput>> {
        let (should_request_clone, receiver) = match self
            .clone_listeners
            .write()
            .expect("RwLock of RemoteAccountClonerClient.clone_listeners is poisoned")
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
