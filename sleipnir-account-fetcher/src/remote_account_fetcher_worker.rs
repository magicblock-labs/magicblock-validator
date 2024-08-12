use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, RwLock},
    vec,
};

use conjunto_transwise::{
    AccountChainSnapshotProvider, AccountChainSnapshotShared,
    DelegationRecordParserImpl, RpcAccountProvider, RpcProviderConfig,
};
use futures_util::future::join_all;
use log::*;
use solana_sdk::pubkey::Pubkey;
use tokio::sync::{
    mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    oneshot::Sender,
};
use tokio_util::sync::CancellationToken;

use crate::{AccountFetcherResult, RemoteAccountFetcherRequest};

pub struct RemoteAccountFetcherWorker {
    account_chain_snapshot_provider: AccountChainSnapshotProvider<
        RpcAccountProvider,
        DelegationRecordParserImpl,
    >,
    request_receiver: UnboundedReceiver<RemoteAccountFetcherRequest>,
    request_sender: UnboundedSender<RemoteAccountFetcherRequest>,
    snapshot_listeners:
        Arc<RwLock<HashMap<Pubkey, Vec<Sender<AccountFetcherResult>>>>>,
    snapshot_results: Arc<RwLock<HashMap<Pubkey, AccountFetcherResult>>>,
}

impl RemoteAccountFetcherWorker {
    pub fn new(config: RpcProviderConfig) -> Self {
        let account_chain_snapshot_provider = AccountChainSnapshotProvider::new(
            RpcAccountProvider::new(config),
            DelegationRecordParserImpl,
        );
        let (request_sender, request_receiver) = unbounded_channel();
        Self {
            account_chain_snapshot_provider,
            request_receiver,
            request_sender,
            snapshot_listeners: Default::default(),
            snapshot_results: Default::default(),
        }
    }

    pub fn get_request_sender(
        &self,
    ) -> UnboundedSender<RemoteAccountFetcherRequest> {
        self.request_sender.clone()
    }

    pub fn get_snapshot_listeners(
        &self,
    ) -> Arc<RwLock<HashMap<Pubkey, Vec<Sender<AccountFetcherResult>>>>> {
        self.snapshot_listeners.clone()
    }

    pub fn get_snapshot_results(
        &self,
    ) -> Arc<RwLock<HashMap<Pubkey, AccountFetcherResult>>> {
        self.snapshot_results.clone()
    }

    pub async fn start_fetchings(
        &mut self,
        cancellation_token: CancellationToken,
    ) {
        loop {
            let mut requests = vec![];
            tokio::select! {
                _ = self.request_receiver.recv_many(&mut requests, 100) => {
                    join_all(
                        requests
                            .into_iter()
                            .map(|request| self.do_fetch(request.account))
                    ).await;
                }
                _ = cancellation_token.cancelled() => {
                    return;
                }
            }
        }
    }

    async fn do_fetch(&self, pubkey: Pubkey) {
        let result = self
            .account_chain_snapshot_provider
            .try_fetch_chain_snapshot_of_pubkey(&pubkey)
            .await
            .map(AccountChainSnapshotShared::from)
            .map_err(|error| error.to_string());

        let listeners = match self
            .snapshot_listeners
            .write()
            .expect(
                "RwLock of RemoteAccountFetcherWorker.snapshot_listeners is poisoned",
            )
            .entry(pubkey)
        {
            // If the entry didn't exist for some reason, something is very wrong, just fail here
            Entry::Vacant(_) => {
                return warn!("Fetch listeners were discarded improperly",);
            }
            // If the entry exists, we want to remove the list of listeners
            Entry::Occupied(entry) => entry.remove(),
        };

        match self
            .snapshot_results
            .write()
            .expect(
                "RwLock of RemoteAccountFetcherWorker.snapshot_results is poisoned",
            )
            .entry(pubkey)
        {
            Entry::Occupied(mut entry) => { entry.get_mut().clone_from(&result); },
            Entry::Vacant(entry) => { entry.insert(result.clone()); },
        };

        for listener in listeners {
            if let Err(error) = listener.send(result.clone()) {
                warn!("Could not send fetch result to listener: {:?}", error);
            }
        }
    }
}
