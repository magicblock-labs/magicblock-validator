use std::{
    cmp::{max, min},
    collections::{hash_map::Entry, HashMap},
    future::Future,
    pin::Pin,
    sync::{Arc, RwLock},
};

use futures_util::{stream::FuturesUnordered, Stream, StreamExt};
use log::*;
use magicblock_metrics::metrics;
use solana_account_decoder::{UiAccount, UiAccountEncoding, UiDataSliceConfig};
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client_api::{config::RpcAccountInfoConfig, response::Response};
use solana_sdk::{
    clock::{Clock, Slot},
    commitment_config::{CommitmentConfig, CommitmentLevel},
    pubkey::Pubkey,
    sysvar::clock,
};
use thiserror::Error;
use tokio::sync::mpsc::Receiver;
use tokio_stream::StreamMap;
use tokio_util::sync::CancellationToken;

type BoxFn = Box<
    dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> + Send,
>;

type SubscriptionStream =
    Pin<Box<dyn Stream<Item = Response<UiAccount>> + Send + 'static>>;

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
    url: String,
    commitment: Option<CommitmentLevel>,
    monitoring_request_receiver: Receiver<(Pubkey, bool)>,
    first_subscribed_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
    last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
}

impl RemoteAccountUpdatesShard {
    pub fn new(
        shard_id: String,
        url: String,
        commitment: Option<CommitmentLevel>,
        monitoring_request_receiver: Receiver<(Pubkey, bool)>,
        first_subscribed_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
        last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
    ) -> Self {
        Self {
            shard_id,
            url,
            commitment,
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
        let ws_url = self.url.as_str();
        // For every account, we only want the updates, not the actual content of the accounts
        let config = RpcAccountInfoConfig {
            commitment: self
                .commitment
                .map(|commitment| CommitmentConfig { commitment }),
            encoding: Some(UiAccountEncoding::Base64),
            data_slice: Some(UiDataSliceConfig {
                offset: 0,
                length: 0,
            }),
            min_context_slot: None,
        };
        let mut pool = PubsubPool::new(ws_url, config).await?;
        // Subscribe to the clock from the RPC (to figure out the latest slot)
        let mut clock_stream = pool.subscribe(clock::ID).await?;
        let mut clock_slot = 0;
        // We'll store useful maps for each of the account subscriptions
        let mut account_streams = StreamMap::new();
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
                    self.try_to_override_last_known_update_slot(clock::ID, clock_slot);
                }
                // When we receive a message to start monitoring an account
                Some((pubkey, unsub)) = self.monitoring_request_receiver.recv() => {
                    if unsub {
                        account_streams.remove(&pubkey);
                        metrics::set_subscriptions_count(account_streams.len(), &self.shard_id);
                        pool.unsubscribe(&pubkey).await;
                        continue;
                    }
                    if pool.subscribed(&pubkey) {
                        continue;
                    }
                    debug!(
                        "Shard {}: Account monitoring started: {:?}, clock_slot: {:?}",
                        self.shard_id,
                        pubkey,
                        clock_slot
                    );
                    let stream = pool
                        .subscribe(pubkey)
                        .await?;
                    account_streams.insert(pubkey, stream);
                    metrics::set_subscriptions_count(account_streams.len(), &self.shard_id);
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
        drop(account_streams);
        drop(clock_stream);
        pool.shutdown().await;
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

struct PubsubPool {
    clients: Vec<PubSubConnection>,
    unsubscribes: HashMap<Pubkey, (usize, BoxFn)>,
    config: RpcAccountInfoConfig,
}

impl PubsubPool {
    async fn new(
        url: &str,
        config: RpcAccountInfoConfig,
    ) -> Result<Self, RemoteAccountUpdatesShardError> {
        // 8 is pretty much arbitrary, but a sane value for the number
        // of connections per RPC upstream, we don't overcomplicate things
        // here, as the whole cloning pipeline will be rewritten quite soon
        const CONNECTIONS_PER_POOL: usize = 8;
        let mut clients = Vec::with_capacity(CONNECTIONS_PER_POOL);
        let mut connections: FuturesUnordered<_> = (0..CONNECTIONS_PER_POOL)
            .map(|_| PubSubConnection::new(url))
            .collect();
        while let Some(c) = connections.next().await {
            clients.push(c?);
        }
        Ok(Self {
            clients,
            unsubscribes: HashMap::new(),
            config,
        })
    }

    async fn subscribe(
        &mut self,
        pubkey: Pubkey,
    ) -> Result<SubscriptionStream, RemoteAccountUpdatesShardError> {
        let (index, client) = self
            .clients
            .iter_mut()
            .enumerate()
            .min_by(|a, b| a.1.subs.cmp(&b.1.subs))
            .expect("clients vec is always greater than 0");
        let (stream, unsubscribe) = client
            .inner
            .account_subscribe(&pubkey, Some(self.config.clone()))
            .await
            .map_err(RemoteAccountUpdatesShardError::PubsubClientError)?;
        client.subs += 1;
        // SAFETY:
        // we never drop the PubsubPool before the returned subscription stream
        // so the lifetime of the stream can be safely extended to 'static
        #[allow(clippy::missing_transmute_annotations)]
        let stream = unsafe { std::mem::transmute(stream) };
        self.unsubscribes.insert(pubkey, (index, unsubscribe));
        Ok(stream)
    }

    async fn unsubscribe(&mut self, pubkey: &Pubkey) {
        let Some((index, callback)) = self.unsubscribes.remove(pubkey) else {
            return;
        };
        callback().await;
        let Some(client) = self.clients.get_mut(index) else {
            return;
        };
        client.subs = client.subs.saturating_sub(1);
    }

    fn subscribed(&mut self, pubkey: &Pubkey) -> bool {
        self.unsubscribes.contains_key(pubkey)
    }

    async fn shutdown(&mut self) {
        // Cleanup all subscriptions and wait for proper shutdown
        for (pubkey, (_, callback)) in self.unsubscribes.drain() {
            info!("Account monitoring killed: {:?}", pubkey);
            callback().await;
        }
        for client in self.clients.drain(..) {
            let _ = client.inner.shutdown().await;
        }
    }
}

struct PubSubConnection {
    inner: PubsubClient,
    subs: usize,
}

impl PubSubConnection {
    async fn new(url: &str) -> Result<Self, RemoteAccountUpdatesShardError> {
        let inner = PubsubClient::new(url)
            .await
            .map_err(RemoteAccountUpdatesShardError::PubsubClientError)?;
        Ok(Self { inner, subs: 0 })
    }
}
