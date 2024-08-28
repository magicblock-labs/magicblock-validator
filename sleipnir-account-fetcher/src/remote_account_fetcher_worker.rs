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

use crate::{AccountFetcherError, AccountFetcherResult};

pub struct RemoteAccountFetcherWorker {
    account_chain_snapshot_provider: AccountChainSnapshotProvider<
        RpcAccountProvider,
        DelegationRecordParserImpl,
    >,
    fetch_request_receiver: UnboundedReceiver<Pubkey>,
    fetch_request_sender: UnboundedSender<Pubkey>,
    fetch_result_listeners:
        Arc<RwLock<HashMap<Pubkey, Vec<Sender<AccountFetcherResult>>>>>,
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
            fetch_result_listeners: Default::default(),
        }
    }

    pub fn get_fetch_request_sender(&self) -> UnboundedSender<Pubkey> {
        self.fetch_request_sender.clone()
    }

    pub fn get_fetch_result_listeners(
        &self,
    ) -> Arc<RwLock<HashMap<Pubkey, Vec<Sender<AccountFetcherResult>>>>> {
        self.fetch_result_listeners.clone()
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
        // Actually fetch the account asynchronously
        let result = match self
            .account_chain_snapshot_provider
            .try_fetch_chain_snapshot_of_pubkey(&pubkey)
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
        // Collect the listeners waiting for the result
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
                return error!("Fetch listeners were discarded improperly: {}", pubkey);
            }
            // If the entry exists, we want to consume the list of listeners
            Entry::Occupied(entry) => entry.remove(),
        };
        // Notify the listeners of the arrival of the result
        for listener in listeners {
            if let Err(error) = listener.send(result.clone()) {
                error!("Could not send fetch resut: {}: {:?}", pubkey, error);
            }
        }
    }
}
