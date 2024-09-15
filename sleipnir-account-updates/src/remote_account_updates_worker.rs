use std::{
    cmp::max,
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, RwLock},
};

use conjunto_transwise::RpcProviderConfig;
use futures_util::StreamExt;
use log::*;
use solana_account_decoder::UiDataSliceConfig;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client_api::config::RpcAccountInfoConfig;
use solana_sdk::{
    clock::Slot, commitment_config::CommitmentConfig, pubkey::Pubkey,
};
use thiserror::Error;
use tokio::sync::mpsc::{
    unbounded_channel, UnboundedReceiver, UnboundedSender,
};
use tokio_stream::StreamMap;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum RemoteAccountUpdatesWorkerError {
    #[error(transparent)]
    PubsubClientError(
        #[from]
        solana_pubsub_client::nonblocking::pubsub_client::PubsubClientError,
    ),
}

pub struct RemoteAccountUpdatesWorker {
    rpc_provider_config: RpcProviderConfig,
    monitoring_request_receiver: UnboundedReceiver<Pubkey>,
    monitoring_request_sender: UnboundedSender<Pubkey>,
    last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
}

impl RemoteAccountUpdatesWorker {
    pub fn new(rpc_provider_config: RpcProviderConfig) -> Self {
        let (monitoring_request_sender, monitoring_request_receiver) =
            unbounded_channel();
        Self {
            rpc_provider_config,
            monitoring_request_receiver,
            monitoring_request_sender,
            last_known_update_slots: Default::default(),
        }
    }

    pub fn get_monitoring_request_sender(&self) -> UnboundedSender<Pubkey> {
        self.monitoring_request_sender.clone()
    }

    pub fn get_last_known_update_slots(
        &self,
    ) -> Arc<RwLock<HashMap<Pubkey, Slot>>> {
        self.last_known_update_slots.clone()
    }

    pub async fn start_monitoring_request_processing(
        &mut self,
        cancellation_token: CancellationToken,
    ) -> Result<(), RemoteAccountUpdatesWorkerError> {
        // Create a pubsub client
        let pubsub_client =
            PubsubClient::new(self.rpc_provider_config.ws_url())
                .await
                .map_err(RemoteAccountUpdatesWorkerError::PubsubClientError)?;
        // For every account, we only want the updates, not the actual content of the accounts
        let rpc_account_info_config = RpcAccountInfoConfig {
            commitment: self
                .rpc_provider_config
                .commitment()
                .map(|commitment| CommitmentConfig { commitment }),
            encoding: None,
            data_slice: Some(UiDataSliceConfig {
                offset: 0,
                length: 0,
            }),
            min_context_slot: None,
        };
        // We'll store useful maps for each of the subscriptions
        let mut streams = StreamMap::new();
        let mut unsubscribes = HashMap::new();
        // Loop forever until we stop the worker
        loop {
            tokio::select! {
                // When we receive a message to start monitoring an account
                Some(account) = self.monitoring_request_receiver.recv() => {
                    if unsubscribes.contains_key(&account) {
                        continue;
                    }
                    info!("Account monitoring started: {}", account);
                    let (stream, unsubscribe) = pubsub_client
                        .account_subscribe(&account, Some(rpc_account_info_config.clone()))
                        .await
                        .map_err(RemoteAccountUpdatesWorkerError::PubsubClientError)?;
                    streams.insert(account, stream);
                    unsubscribes.insert(account, unsubscribe);
                }
                // When we receive an update from any account subscriptions
                Some((account, update)) = streams.next() => {
                    let current_update_slot = update.context.slot;
                    info!(
                        "Account update received: {}, in slot: {}",
                        account, current_update_slot
                    );
                    match self.last_known_update_slots
                        .write()
                        .expect("RwLock of RemoteAccountUpdatesWorker.last_known_update_slots poisoned")
                        .entry(account)
                    {
                        Entry::Vacant(entry) => {
                            entry.insert(current_update_slot);
                        }
                        Entry::Occupied(mut entry) => {
                            *entry.get_mut() = max(*entry.get(), current_update_slot);
                        }
                    }
                }
                // When we want to stop the worker (it was cancelled)
                _ = cancellation_token.cancelled() => {
                    break;
                }
            }
        }
        // Cleanup all subscriptions and wait for proper shutdown
        for unsubscribe in unsubscribes.into_values() {
            unsubscribe().await;
        }
        drop(streams);
        pubsub_client.shutdown().await?;
        // Done
        Ok(())
    }
}
