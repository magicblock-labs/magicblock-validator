use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, Mutex},
};

use conjunto_transwise::{
    AccountChainSnapshotProvider, AccountChainSnapshotShared,
    DelegationRecordParserImpl, RpcAccountProvider, RpcProviderConfig,
};
use log::*;
use solana_sdk::pubkey::Pubkey;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;

use crate::{RemoteAccountFetcherRequest, RemoteAccountFetcherResponse};

pub struct RemoteAccountFetcherWorker {
    account_chain_snapshot_provider: AccountChainSnapshotProvider<
        RpcAccountProvider,
        DelegationRecordParserImpl,
    >,
    request_receiver: UnboundedReceiver<RemoteAccountFetcherRequest>,
    request_sender: UnboundedSender<RemoteAccountFetcherRequest>,
    responses: Arc<Mutex<HashMap<Pubkey, RemoteAccountFetcherResponse>>>,
}

impl RemoteAccountFetcherWorker {
    pub fn new(config: RpcProviderConfig) -> Self {
        let account_chain_snapshot_provider = AccountChainSnapshotProvider::new(
            RpcAccountProvider::new(config),
            DelegationRecordParserImpl,
        );
        let (request_sender, request_receiver) = mpsc::unbounded_channel();
        Self {
            account_chain_snapshot_provider,
            request_receiver,
            request_sender,
            responses: Default::default(),
        }
    }

    pub fn get_request_sender(
        &self,
    ) -> UnboundedSender<RemoteAccountFetcherRequest> {
        self.request_sender.clone()
    }
    pub fn get_responses(
        &self,
    ) -> Arc<Mutex<HashMap<Pubkey, RemoteAccountFetcherResponse>>> {
        self.responses.clone()
    }

    pub async fn start_fetchings(
        &mut self,
        cancellation_token: CancellationToken,
    ) {
        loop {
            tokio::select! {
                Some(request) = self.request_receiver.recv() => {
                    self.do_fetch(request.account).await
                }
                _ = cancellation_token.cancelled() => {
                    return;
                }
            }
        }
    }

    async fn do_fetch(&self, pubkey: Pubkey) {
        // Schedule the fetch, and transform the result into a cloneable type
        let result = self
            .account_chain_snapshot_provider
            .try_fetch_chain_snapshot_of_pubkey(&pubkey)
            .await
            .map(AccountChainSnapshotShared::from)
            .map_err(|error| error.to_string());
        // Lock the query_cache to update the result of the fetch and get the list of listeners
        let listeners = match self
            .responses
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
                // Get the list of listeners bu replacing the content of the existing entry with the fetch result
                match entry.insert(RemoteAccountFetcherResponse::Available {
                    result: result.clone(),
                }) {
                    RemoteAccountFetcherResponse::Available { .. } => vec![],
                    RemoteAccountFetcherResponse::InFlight { listeners } => {
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
