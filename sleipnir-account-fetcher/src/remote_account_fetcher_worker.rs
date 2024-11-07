use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, RwLock},
    vec,
};

use conjunto_transwise::{
    AccountChainSnapshot, AccountChainSnapshotProvider,
    AccountChainSnapshotShared, DelegationRecordParserImpl, RpcAccountProvider,
    RpcProviderConfig,
};
use futures_util::future::join_all;
use log::*;
use solana_sdk::pubkey::Pubkey;
use tokio::sync::mpsc::{
    unbounded_channel, UnboundedReceiver, UnboundedSender,
};
use tokio_util::sync::CancellationToken;

use crate::{AccountFetcherError, AccountFetcherListeners};

pub struct RemoteAccountFetcherWorker {
    account_chain_snapshot_providers: Vec<
        AccountChainSnapshotProvider<
            RpcAccountProvider,
            DelegationRecordParserImpl,
        >,
    >,
    fetch_request_receiver: UnboundedReceiver<Pubkey>,
    fetch_request_sender: UnboundedSender<Pubkey>,
    fetch_listeners: Arc<RwLock<HashMap<Pubkey, AccountFetcherListeners>>>,
}

impl RemoteAccountFetcherWorker {
    pub fn new(rpc_provider_configs: Vec<RpcProviderConfig>) -> Self {
        let account_chain_snapshot_providers = rpc_provider_configs
            .into_iter()
            .map(|rpc_provider_config| {
                AccountChainSnapshotProvider::new(
                    RpcAccountProvider::new(rpc_provider_config),
                    DelegationRecordParserImpl,
                )
            })
            .collect();
        let (fetch_request_sender, fetch_request_receiver) =
            unbounded_channel();
        Self {
            account_chain_snapshot_providers,
            fetch_request_receiver,
            fetch_request_sender,
            fetch_listeners: Default::default(),
        }
    }

    pub fn get_fetch_request_sender(&self) -> UnboundedSender<Pubkey> {
        self.fetch_request_sender.clone()
    }

    pub fn get_fetch_listeners(
        &self,
    ) -> Arc<RwLock<HashMap<Pubkey, AccountFetcherListeners>>> {
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

    async fn process_fetch_request(&self, pubkey: Pubkey) {
        // Actually fetch the account asynchronously from all RPCs
        let results =
            join_all(self.account_chain_snapshot_providers.iter().map(
                |account_chain_snapshot_provider| {
                    account_chain_snapshot_provider
                        .try_fetch_chain_snapshot_of_pubkey(&pubkey)
                },
            ))
            .await;
        // Select the best result from all the fetches
        let result = self.pick_the_best_fetch_result(&pubkey, results);
        // Log the result for debugging purposes
        debug!("Account fetch: {:?}, snapshot: {:?}", pubkey, result);
        // Collect the listeners waiting for the result
        let listeners = match self
            .fetch_listeners
            .write()
            .expect(
                "RwLock of RemoteAccountFetcherWorker.fetch_listeners is poisoned",
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

    fn pick_the_best_fetch_result(
        &self,
        pubkey: &Pubkey,
        fetch_results: Vec<LockboxResult<AccountChainSnapshot>>,
    ) -> LockboxResult<AccountChainSnapshotShared> {
        // Picked the best result out of all fetch results from all RPCs
        let mut best_result = Err(AccountFetcherError::FailedToFetch(
            "No fetch was initiated".to_string(),
        ));
        // Check all fetch results one by one
        for fetch_result in fetch_results {
            match fetch_result {
                // If the fetch has succeeded
                Ok(fetch_snapshot) => match best_result {
                    // If the best fetch has succeeded, only replace it if the slot is more recent
                    Ok(best_snapshot) => {
                        if fetch_snapshot.at_slot > best_snapshot.at_slot {
                            best_result = Ok(AccountChainSnapshotShared::from(
                                fetch_snapshot,
                            ))
                        }
                    }
                    // If the best fetch has failed, we replace it with the current success
                    Err(error) => {
                        best_result =
                            Ok(AccountChainSnapshotShared::from(fetch_snapshot))
                    }
                },
                Err(error) => {
                    // Log the error now, since we're going to lose the stacktrace after string conversion
                    warn!("Failed to fetch account: {} :{:?}", pubkey, error);
                    // We ignore the error if another fetch already succeeded
                    if best_result.is_err() {
                        // LockboxError is unclonable, so we have to downgrade it to a clonable error type
                        best_result = Err(AccountFetcherError::FailedToFetch(
                            error.to_string(),
                        ));
                    }
                }
            }
        }
        // Use the best result and discard all other fetches
        best_result
    }
}
