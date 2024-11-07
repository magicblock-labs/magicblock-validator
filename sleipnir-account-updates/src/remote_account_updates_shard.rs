use std::{
    cmp::max,
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, RwLock},
};

use conjunto_transwise::RpcProviderConfig;
use futures_util::StreamExt;
use log::*;
use solana_account_decoder::{UiAccount, UiAccountEncoding};
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client_api::config::RpcAccountInfoConfig;
use solana_sdk::{
    clock::Slot, commitment_config::CommitmentConfig, pubkey::Pubkey,
};
use thiserror::Error;
use tokio::sync::mpsc::UnboundedReceiver;
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
    monitoring_request_receiver: UnboundedReceiver<Pubkey>,
    last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
}

impl RemoteAccountUpdatesShard {
    pub fn new(
        shard_id: String,
        rpc_provider_config: RpcProviderConfig,
        monitoring_request_receiver: UnboundedReceiver<Pubkey>,
        last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
    ) -> Self {
        Self {
            shard_id,
            rpc_provider_config,
            monitoring_request_receiver,
            last_known_update_slots,
        }
    }

    pub async fn start_monitoring_request_processing(
        &mut self,
        cancellation_token: CancellationToken,
    ) -> Result<(), RemoteAccountUpdatesShardError> {
        // Create a pubsub client
        info!("Shard {}: Starting", self.shard_id);
        let pubsub_client =
            PubsubClient::new(self.rpc_provider_config.ws_url())
                .await
                .map_err(RemoteAccountUpdatesShardError::PubsubClientError)?;
        // Use the inneficient encoding, so that the RPC doesn't try to send the big accounts's data
        let encoding = Some(UiAccountEncoding::Base58);
        // Since we want to check for data duplicate, we need to listen to all the accounts's data
        let data_slice = None;
        // Generate the RPC config
        let rpc_account_info_config = RpcAccountInfoConfig {
            commitment: self
                .rpc_provider_config
                .commitment()
                .map(|commitment| CommitmentConfig { commitment }),
            encoding,
            data_slice,
            min_context_slot: None,
        };
        // We'll keep a cache of all the account data that we last received, so we can check for changes and ignore duplicate updates
        let mut last_known_update_accounts = HashMap::new();
        // We'll store useful maps for each of the subscriptions
        let mut streams = StreamMap::new();
        let mut unsubscribes = HashMap::new();
        // Loop forever until we stop the worker
        loop {
            tokio::select! {
                // When we receive a message to start monitoring an account
                Some(pubkey) = self.monitoring_request_receiver.recv() => {
                    if unsubscribes.contains_key(&pubkey) {
                        continue;
                    }
                    info!("Shard {}: Account monitoring started: {:?}", self.shard_id, pubkey);
                    let (stream, unsubscribe) = pubsub_client
                        .account_subscribe(&pubkey, Some(rpc_account_info_config.clone()))
                        .await
                        .map_err(RemoteAccountUpdatesShardError::PubsubClientError)?;
                    streams.insert(pubkey, stream);
                    unsubscribes.insert(pubkey, unsubscribe);
                }
                // When we receive an update from any account subscriptions
                Some((pubkey, update)) = streams.next() => {
                    if self.check_if_the_account_may_have_changed(last_known_update_accounts.get(&pubkey), &update.value) {
                        let current_update_slot = update.context.slot;
                        debug!(
                            "Shard {}: Account update: {:?}, at slot: {}, data: {:?}",
                            self.shard_id, pubkey, current_update_slot, update.value.data.decode(),
                        );
                        last_known_update_accounts.insert(pubkey, update.value);
                        self.try_to_override_last_known_update(pubkey, current_update_slot);
                    }
                }
                // When we want to stop the worker (it was cancelled)
                _ = cancellation_token.cancelled() => {
                    break;
                }
            }
        }
        // Cleanup all subscriptions and wait for proper shutdown
        for (pubkey, unsubscribe) in unsubscribes.into_iter() {
            info!(
                "Shard {}: Account monitoring killed: {:?}",
                self.shard_id, pubkey
            );
            unsubscribe().await;
        }
        drop(streams);
        pubsub_client.shutdown().await?;
        info!("Shard {}: Stopped", self.shard_id);
        // Done
        Ok(())
    }

    fn check_if_the_account_may_have_changed(
        &self,
        last_known_update_account: Option<&UiAccount>,
        current_update_account: &UiAccount,
    ) -> bool {
        match last_known_update_account {
            Some(last_known_update_account) => {
                // For account we don't have access to the data, we assume we need a refresh, just in case
                let last_known_update_data =
                    last_known_update_account.data.decode();
                if last_known_update_data.is_none() {
                    return true;
                }
                let current_update_data = current_update_account.data.decode();
                if current_update_data.is_none() {
                    return true;
                }
                if last_known_update_data != current_update_data {
                    return true;
                }
                // Otherwise just check if every other field match previous update
                if last_known_update_account.lamports
                    != current_update_account.lamports
                {
                    return true;
                }
                if last_known_update_account.executable
                    != current_update_account.executable
                {
                    return true;
                }
                if last_known_update_account.owner
                    != current_update_account.owner
                {
                    return true;
                }
                if last_known_update_account.space
                    != current_update_account.space
                {
                    return true;
                }
                if last_known_update_account.rent_epoch
                    != current_update_account.rent_epoch
                {
                    return true;
                }
                // If everything match perfectly, we can safely ignore the current update
                false
            }
            None => true,
        }
    }

    fn try_to_override_last_known_update(
        &self,
        pubkey: Pubkey,
        current_update_slot: Slot,
    ) {
        // We don't need to acquire a write lock if we already know the update is too old
        let last_known_update_slot = self.last_known_update_slots
            .read()
            .expect("RwLock of RemoteAccountUpdatesShard.last_known_update_slots poisoned")
            .get(&pubkey)
            .cloned()
            .unwrap_or(u64::MIN);
        if current_update_slot > last_known_update_slot {
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
