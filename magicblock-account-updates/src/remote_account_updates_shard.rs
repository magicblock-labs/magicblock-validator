use std::{
    cell::RefCell,
    cmp::{max, min},
    collections::{hash_map::Entry, BinaryHeap, HashMap},
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::{Arc, RwLock},
    time::Duration,
};

use futures_util::{stream::FuturesUnordered, FutureExt, Stream, StreamExt};
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
    #[error("failed to subscribe to remote account updates")]
    SubscriptionTimeout,
}

pub struct RemoteAccountUpdatesShard {
    shard_id: String,
    monitoring_request_receiver: Receiver<(Pubkey, bool)>,
    first_subscribed_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
    last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
    pool: PubsubPool,
}

impl RemoteAccountUpdatesShard {
    pub async fn new(
        shard_id: String,
        url: String,
        commitment: Option<CommitmentLevel>,
        monitoring_request_receiver: Receiver<(Pubkey, bool)>,
        first_subscribed_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
        last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
    ) -> Result<Self, RemoteAccountUpdatesShardError> {
        // For every account, we only want the updates, not the actual content of the accounts
        let config = RpcAccountInfoConfig {
            commitment: commitment
                .map(|commitment| CommitmentConfig { commitment }),
            encoding: Some(UiAccountEncoding::Base64),
            data_slice: Some(UiDataSliceConfig {
                offset: 0,
                length: 0,
            }),
            min_context_slot: None,
        };
        // Create a pubsub client
        info!("Shard {}: Starting", shard_id);
        let pool = PubsubPool::new(&url, config).await?;
        Ok(Self {
            shard_id,
            monitoring_request_receiver,
            first_subscribed_slots,
            last_known_update_slots,
            pool,
        })
    }

    pub async fn start_monitoring_request_processing(
        mut self,
        cancellation_token: CancellationToken,
    ) {
        let mut clock_slot = 0;
        // We'll store useful maps for each of the account subscriptions
        let mut account_streams = StreamMap::new();
        const LOG_CLOCK_FREQ: u64 = 100;
        let mut log_clock_count = 0;
        // Subscribe to the clock from the RPC (to figure out the latest slot)
        let subscription = self.pool.subscribe(clock::ID).await;
        let Ok((mut clock_stream, unsub)) = subscription.result else {
            error!("failed to subscribe to clock on shard: {}", self.shard_id);
            return;
        };
        self.pool
            .unsubscribes
            .insert(clock::ID, (subscription.client.subs.clone(), unsub));
        self.pool.clients.push(subscription.client);

        let mut requests = FuturesUnordered::new();
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
                Some((pubkey, unsub)) = self.monitoring_request_receiver.recv(), if !self.pool.is_empty() => {
                    if unsub {
                        account_streams.remove(&pubkey);
                        metrics::set_subscriptions_count(account_streams.len(), &self.shard_id);
                        self.pool.unsubscribe(&pubkey);
                        continue;
                    }
                    if self.pool.subscribed(&pubkey) {
                        continue;
                    }
                    // spawn the actual subscription handling to a background
                    // task, so that the select loop is not blocked by it
                    let sub = self.pool.subscribe(pubkey).map(move |stream| (stream,  pubkey));
                    requests.push(sub);
                }
                Some((result, pubkey)) = requests.next(), if !requests.is_empty() => {
                    let (stream, unsub) = match result.result {
                        Ok(s) => s,
                        Err(e) => {
                            warn!("shard {} failed to websocket subscribe to {pubkey}: {e}", self.shard_id);
                            self.pool.clients.push(result.client);
                            continue;
                        }
                    };
                    self.try_to_override_first_subscribed_slot(pubkey, clock_slot);
                    self.pool.unsubscribes.insert(pubkey, (result.client.subs.clone(), unsub));
                    self.pool.clients.push(result.client);
                    account_streams.insert(pubkey, stream);
                    debug!(
                        "Shard {}: Account monitoring started: {:?}, clock_slot: {:?}",
                        self.shard_id,
                        pubkey,
                        clock_slot
                    );
                    metrics::set_subscriptions_count(account_streams.len(), &self.shard_id);
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
        self.pool.shutdown().await;
        info!("Shard {}: Stopped", self.shard_id);
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
    clients: BinaryHeap<PubSubConnection>,
    unsubscribes: HashMap<Pubkey, (Rc<RefCell<usize>>, BoxFn)>,
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
        let mut clients = BinaryHeap::with_capacity(CONNECTIONS_PER_POOL);
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

    fn subscribe(
        &mut self,
        pubkey: Pubkey,
    ) -> impl Future<Output = SubscriptionResult> {
        let client = self.clients.pop().expect(
            "websocket connection pool always has at least one connection",
        );
        const SUBSCRIPTION_TIMEOUT: Duration = Duration::from_secs(30);
        let config = Some(self.config.clone());
        async move {
            let request = client.inner.account_subscribe(&pubkey, config);
            let request_with_timeout = tokio::time::timeout(SUBSCRIPTION_TIMEOUT, request);
            let Ok(result) = request_with_timeout.await else {
                let result =
                    Err(RemoteAccountUpdatesShardError::SubscriptionTimeout);
                return SubscriptionResult { result, client };
            };
            let result = result
                .map_err(RemoteAccountUpdatesShardError::PubsubClientError)
                .map(|(stream, unsub)| {
                    // SAFETY:
                    // we never drop the PubsubPool before the returned subscription stream
                    // so the lifetime of the stream can be safely extended to 'static
                    #[allow(clippy::missing_transmute_annotations)]
                    let stream = unsafe { std::mem::transmute(stream) };
                    *client.subs.borrow_mut() += 1;
                    (stream, unsub)
                });
            SubscriptionResult { result, client }
        }
    }

    fn unsubscribe(&mut self, pubkey: &Pubkey) {
        let Some((subs, callback)) = self.unsubscribes.remove(pubkey) else {
            return;
        };
        let count = *subs.borrow();
        *subs.borrow_mut() = count.saturating_sub(1);
        drop(subs);
        tokio::spawn(callback());
    }

    fn subscribed(&self, pubkey: &Pubkey) -> bool {
        self.unsubscribes.contains_key(pubkey)
    }

    async fn shutdown(&mut self) {
        // Cleanup all subscriptions and wait for proper shutdown
        for (pubkey, (_, callback)) in self.unsubscribes.drain() {
            debug!("Account monitoring killed: {:?}", pubkey);
            tokio::spawn(callback());
        }
        for client in self.clients.drain() {
            let _ = client.inner.shutdown().await;
        }
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }
}

struct PubSubConnection {
    inner: PubsubClient,
    subs: Rc<RefCell<usize>>,
}

impl PartialEq for PubSubConnection {
    fn eq(&self, other: &Self) -> bool {
        self.subs.eq(&other.subs)
    }
}

impl PartialOrd for PubSubConnection {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        // NOTE: intentional reverse ordering for the use in the BinaryHeap
        Some(other.subs.cmp(&self.subs))
    }
}

impl Eq for PubSubConnection {}

impl Ord for PubSubConnection {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // NOTE: intentional reverse ordering for the use in the BinaryHeap
        other.subs.cmp(&self.subs)
    }
}

impl PubSubConnection {
    async fn new(url: &str) -> Result<Self, RemoteAccountUpdatesShardError> {
        let inner = PubsubClient::new(url)
            .await
            .map_err(RemoteAccountUpdatesShardError::PubsubClientError)?;
        Ok(Self {
            inner,
            subs: Default::default(),
        })
    }
}

// SAFETY: the Rc<RefCell> used in the connection never escape outside of the Shard,
// and the borrows are never held across the await points, thus these impls are safe
unsafe impl Send for PubSubConnection {}
unsafe impl Send for PubsubPool {}
unsafe impl Send for RemoteAccountUpdatesShard {}

struct SubscriptionResult {
    result: Result<(SubscriptionStream, BoxFn), RemoteAccountUpdatesShardError>,
    client: PubSubConnection,
}
