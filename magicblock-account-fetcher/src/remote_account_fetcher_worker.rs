use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, Mutex},
    vec,
};

use conjunto_transwise::{
    AccountChainSnapshotProvider, AccountChainSnapshotShared,
    DelegationRecordParserImpl, RpcAccountProvider, RpcProviderConfig,
};
use futures_util::future::join_all;
use log::*;
use solana_sdk::{clock::Slot, pubkey::Pubkey};
use tokio::sync::mpsc::{
    unbounded_channel, UnboundedReceiver, UnboundedSender,
};
use tokio_util::sync::CancellationToken;

use crate::{AccountFetcherError, AccountFetcherListeners};

pub struct RemoteAccountFetcherWorker {
    account_chain_snapshot_provider: AccountChainSnapshotProvider<
        RpcAccountProvider,
        DelegationRecordParserImpl,
    >,
    fetch_request_receiver: UnboundedReceiver<(Pubkey, Option<Slot>)>,
    fetch_request_sender: UnboundedSender<(Pubkey, Option<Slot>)>,
    fetch_listeners: Arc<Mutex<HashMap<Pubkey, AccountFetcherListeners>>>,
}

impl RemoteAccountFetcherWorker {
    pub fn new(config: RpcProviderConfig) -> Self {
        let account_chain_snapshot_provider = AccountChainSnapshotProvider::new(
            RpcAccountProvider::new(config),
            DelegationRecordParserImpl,
        );
        let (fetch_request_sender, fetch_request_receiver) =
            unbounded_channel();
        Self {
            account_chain_snapshot_provider,
            fetch_request_receiver,
            fetch_request_sender,
            fetch_listeners: Default::default(),
        }
    }

    pub fn get_fetch_request_sender(
        &self,
    ) -> UnboundedSender<(Pubkey, Option<Slot>)> {
        self.fetch_request_sender.clone()
    }

    pub fn get_fetch_listeners(
        &self,
    ) -> Arc<Mutex<HashMap<Pubkey, AccountFetcherListeners>>> {
        self.fetch_listeners.clone()
    }

    pub async fn start_fetch_request_processing(
        &mut self,
        cancellation_token: CancellationToken,
    ) {
        loop {
            let mut requests = vec![];
            tokio::select! {
                _ = self.fetch_request_receiver.recv_many(&mut requests, 100) => {
                    join_all(
                        requests
                            .into_iter()
                            .map(|request| self.process_fetch_request(request))
                    ).await;
                }
                _ = cancellation_token.cancelled() => {
                    return;
                }
            }
        }
    }

    async fn process_fetch_request(&self, request: (Pubkey, Option<Slot>)) {
        let pubkey = request.0;
        let min_context_slot = request.1;
        // Actually fetch the account asynchronously
        let result = match self
            .account_chain_snapshot_provider
            .try_fetch_chain_snapshot_of_pubkey(&pubkey, min_context_slot)
            .await
        {
            Ok(snapshot) => Ok(AccountChainSnapshotShared::from(snapshot)),
            // LockboxError is unclonable, so we have to downgrade it to a clonable error type
            Err(error) => {
                // Log the error now, since we're going to lose the stacktrace after string conversion
                warn!("Failed to fetch account: {} :{:?}", pubkey, error);
                // Lose the error full stack trace and create a simplified clonable string version
                Err(AccountFetcherError::FailedToFetch(error.to_string()))
            }
        };
        // Log the result for debugging purposes
        debug!(
            "Account fetch: {:?}, min_context_slot: {:?}, snapshot: {:?}",
            pubkey, min_context_slot, result
        );
        // Collect the listeners waiting for the result
        let listeners = match self
            .fetch_listeners
            .lock()
            .expect(
                "Mutex of RemoteAccountFetcherWorker.fetch_listeners is poisoned",
            )
            .entry(pubkey)
        {
            // If the entry didn't exist for some reason, something is very wrong, just fail here
            Entry::Vacant(_) => {
                return error!("Fetch listeners were discarded improperly: {}", pubkey);
            }
            // If the entry exists, we want to consume the list of listeners
            Entry::Occupied(entry) => entry.remove(),
        };
        // Notify the listeners of the arrival of the result
        for listener in listeners {
            if let Err(error) = listener.send(result.clone()) {
                error!("Could not send fetch result: {}: {:?}", pubkey, error);
            }
        }
    }
}
