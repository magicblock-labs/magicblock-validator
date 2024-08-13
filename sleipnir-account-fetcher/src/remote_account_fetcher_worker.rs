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

use crate::AccountFetcherResult;

pub struct RemoteAccountFetcherWorker {
    account_chain_snapshot_provider: AccountChainSnapshotProvider<
        RpcAccountProvider,
        DelegationRecordParserImpl,
    >,
    request_receiver: UnboundedReceiver<Pubkey>,
    request_sender: UnboundedSender<Pubkey>,
    fetch_result_listeners:
        Arc<RwLock<HashMap<Pubkey, Vec<Sender<AccountFetcherResult>>>>>,
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
            fetch_result_listeners: Default::default(),
        }
    }

    pub fn get_request_sender(&self) -> UnboundedSender<Pubkey> {
        self.request_sender.clone()
    }

    pub fn get_fetch_result_listeners(
        &self,
    ) -> Arc<RwLock<HashMap<Pubkey, Vec<Sender<AccountFetcherResult>>>>> {
        self.fetch_result_listeners.clone()
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
                            .map(|request| self.do_fetch(request))
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
            .fetch_result_listeners
            .write()
            .expect(
                "RwLock of RemoteAccountFetcherWorker.fetch_result_listeners is poisoned",
            )
            .entry(pubkey)
        {
            // If the entry didn't exist for some reason, something is very wrong, just fail here
            Entry::Vacant(_) => {
                return warn!("Fetch listeners were discarded improperly",);
            }
            // If the entry exists, we want to consume the list of listeners
            Entry::Occupied(entry) => entry.remove(),
        };
        for listener in listeners {
            if let Err(error) = listener.send(result.clone()) {
                warn!("Could not send fetch result to listener: {:?}", error);
            }
        }
    }
}
