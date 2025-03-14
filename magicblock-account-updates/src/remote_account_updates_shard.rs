use std::{
    cmp::{max, min},
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, RwLock},
};

use conjunto_transwise::RpcProviderConfig;
use futures_util::StreamExt;
use log::*;
use solana_account_decoder::{UiAccountEncoding, UiDataSliceConfig};
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client_api::config::RpcAccountInfoConfig;
use solana_sdk::{
    clock::{Clock, Slot},
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    sysvar::clock,
};
use thiserror::Error;
use tokio::sync::mpsc::Receiver;
use tokio_stream::StreamMap;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum RemoteAccountUpdatesShardError {
    #[error(transparent)]
    PubsubClientError(
        #[from]
        solana_pubsub_client::nonblocking::pubsub_client::PubsubClientError,
    ),
}

pub struct RemoteAccountUpdatesShard {
    shard_id: String,
    rpc_provider_config: RpcProviderConfig,
    monitoring_request_receiver: Receiver<Pubkey>,
    first_subscribed_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
    last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
}

impl RemoteAccountUpdatesShard {
    pub fn new(
        shard_id: String,
        rpc_provider_config: RpcProviderConfig,
        monitoring_request_receiver: Receiver<Pubkey>,
        first_subscribed_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
        last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
    ) -> Self {
        Self {
            shard_id,
            rpc_provider_config,
            monitoring_request_receiver,
            first_subscribed_slots,
            last_known_update_slots,
        }
    }

    pub async fn start_monitoring_request_processing(
        &mut self,
        cancellation_token: CancellationToken,
    ) -> Result<(), RemoteAccountUpdatesShardError> {
        // Create a pubsub client
        info!("Shard {}: Starting", self.shard_id);
        let ws_url = self.rpc_provider_config.ws_url();
        let pubsub_client = PubsubClient::new(ws_url)
            .await
            .map_err(RemoteAccountUpdatesShardError::PubsubClientError)?;
        // For every account, we only want the updates, not the actual content of the accounts
        let rpc_account_info_config = Some(RpcAccountInfoConfig {
            commitment: self
                .rpc_provider_config
                .commitment()
                .map(|commitment| CommitmentConfig { commitment }),
            encoding: Some(UiAccountEncoding::Base64),
            data_slice: Some(UiDataSliceConfig {
                offset: 0,
                length: 0,
            }),
            min_context_slot: None,
        });
        // Subscribe to the clock from the RPC (to figure out the latest slot)
        let (mut clock_stream, clock_unsubscribe) = pubsub_client
            .account_subscribe(&clock::ID, rpc_account_info_config.clone())
            .await
            .map_err(RemoteAccountUpdatesShardError::PubsubClientError)?;
        let mut clock_slot = 0;
        // We'll store useful maps for each of the account subscriptions
        let mut account_streams = StreamMap::new();
        let mut account_unsubscribes = HashMap::new();
        const LOG_CLOCK_FREQ: u64 = 100;
        let mut log_clock_count = 0;

        // Loop forever until we stop the worker
        loop {
            tokio::select! {
                // When we receive a new clock notification
                Some(clock_update) = clock_stream.next() => {
                    log_clock_count += 1;
                    let clock_data = clock_update.value.data.decode();
                    if let Some(clock_data) = clock_data {
                        let clock_value = bincode::deserialize::<Clock>(&clock_data);
                        if log_clock_count % LOG_CLOCK_FREQ == 0 {
                            trace!("Shard {}: received: {}th clock value {:?}", log_clock_count, self.shard_id, clock_value);
                        }
                        if let Ok(clock_value) = clock_value {
                            clock_slot = clock_value.slot;
                        } else {
                            warn!("Shard {}: Failed to deserialize clock data: {:?}", self.shard_id, clock_data);
                        }
                    } else {
                        warn!("Shard {}: Received empty clock data", self.shard_id);
                    }
                }
                // When we receive a message to start monitoring an account
                Some(pubkey) = self.monitoring_request_receiver.recv() => {
                    if account_unsubscribes.contains_key(&pubkey) {
                        continue;
                    }
                    debug!(
                        "Shard {}: Account monitoring started: {:?}, clock_slot: {:?}",
                        self.shard_id,
                        pubkey,
                        clock_slot
                    );
                    let (stream, unsubscribe) = pubsub_client
                        .account_subscribe(&pubkey, rpc_account_info_config.clone())
                        .await
                        .map_err(RemoteAccountUpdatesShardError::PubsubClientError)?;
                    account_streams.insert(pubkey, stream);
                    account_unsubscribes.insert(pubkey, unsubscribe);
                    self.try_to_override_first_subscribed_slot(pubkey, clock_slot);
                }
                // When we receive an update from any account subscriptions
                Some((pubkey, update)) = account_streams.next() => {
                    let current_update_slot = update.context.slot;
                    debug!(
                        "Shard {}: Account update: {:?}, current_update_slot: {}, data: {:?}",
                        self.shard_id, pubkey, current_update_slot, update.value.data.decode(),
                    );
                    self.try_to_override_last_known_update_slot(pubkey, current_update_slot);
                }
                // When we want to stop the worker (it was cancelled)
                _ = cancellation_token.cancelled() => {
                    break;
                }
            }
        }
        // Cleanup all subscriptions and wait for proper shutdown
        for (pubkey, account_unsubscribes) in account_unsubscribes.into_iter() {
            info!(
                "Shard {}: Account monitoring killed: {:?}",
                self.shard_id, pubkey
            );
            account_unsubscribes().await;
        }
        clock_unsubscribe().await;
        drop(account_streams);
        drop(clock_stream);
        pubsub_client.shutdown().await?;
        info!("Shard {}: Stopped", self.shard_id);
        // Done
        Ok(())
    }

    fn try_to_override_first_subscribed_slot(
        &self,
        pubkey: Pubkey,
        subscribed_slot: Slot,
    ) {
        // We don't need to acquire a write lock if we already know the slot is already recent enough
        let first_subscribed_slot = self.first_subscribed_slots
            .read()
            .expect("RwLock of RemoteAccountUpdatesShard.first_subscribed_slots poisoned")
            .get(&pubkey)
            .cloned();
        if subscribed_slot < first_subscribed_slot.unwrap_or(u64::MAX) {
            // If the subscribe slot seems to be the oldest one, we need to acquire a write lock to update it
            match self.first_subscribed_slots
                .write()
                .expect("RwLock of RemoteAccountUpdatesShard.first_subscribed_slots poisoned")
                .entry(pubkey)
            {
                Entry::Vacant(entry) => {
                    entry.insert(subscribed_slot);
                }
                Entry::Occupied(mut entry) => {
                    *entry.get_mut() = min(*entry.get(), subscribed_slot);
                }
            }
        }
    }

    fn try_to_override_last_known_update_slot(
        &self,
        pubkey: Pubkey,
        current_update_slot: Slot,
    ) {
        // We don't need to acquire a write lock if we already know the update is too old
        let last_known_update_slot = self.last_known_update_slots
            .read()
            .expect("RwLock of RemoteAccountUpdatesShard.last_known_update_slots poisoned")
            .get(&pubkey)
            .cloned();
        if current_update_slot > last_known_update_slot.unwrap_or(u64::MIN) {
            // If the current update seems to be the most recent one, we need to acquire a write lock to update it
            match self.last_known_update_slots
                .write()
                .expect("RwLock of RemoteAccountUpdatesShard.last_known_update_slots poisoned")
                .entry(pubkey)
            {
                Entry::Vacant(entry) => {
                    entry.insert(current_update_slot);
                }
                Entry::Occupied(mut entry) => {
                    *entry.get_mut() = max(*entry.get(), current_update_slot);
                }
            }
        }
    }
}
