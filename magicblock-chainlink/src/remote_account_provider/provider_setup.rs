//! Provider construction: endpoint selection, transport policy, pubsub wiring.

use std::{
    collections::{HashMap, HashSet},
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    time::Duration,
};

pub(crate) use chain_pubsub_client::ChainPubsubClient;
pub(crate) use chain_rpc_client::{ChainRpcClient, ChainRpcClientImpl};
use chain_slot::ChainSlot;
use config::RemoteAccountProviderConfig;
pub(crate) use errors::{
    RemoteAccountProviderError, RemoteAccountProviderResult,
};
use futures_util::future::{join_all, try_join_all};
use lru_cache::TieredSubscribedAccountsTracker;
use magicblock_config::config::SubscriptionTransport;
use magicblock_metrics::{
    metrics,
    metrics::{
        set_monitored_accounts_count, AccountFetchContext, AccountFetchReason,
    },
};
pub(crate) use remote_account::RemoteAccount;
use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::RpcProgramAccountsConfig;
use solana_sdk_ids::sysvar::clock;
use tokio::{
    sync::{mpsc, Mutex as AsyncMutex, Notify},
    task, time,
};
use tracing::*;

use super::*;
use crate::{
    errors::ChainlinkResult,
    remote_account_provider::chain_updates_client::ChainUpdatesClient,
    submux::SubMuxClient,
};

impl<T: ChainRpcClient, U: ChainPubsubClient> RemoteAccountProvider<T, U> {
    pub async fn try_from_clients_and_mode(
        rpc_client: T,
        pubsub_client: U,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
        lrucache_subscribed_accounts: Arc<AccountsLruCache>,
        chain_slot: Arc<AtomicU64>,
    ) -> ChainlinkResult<Option<RemoteAccountProvider<T, U>>> {
        let chain_slot = ChainSlot::new(chain_slot);
        if config.lifecycle_mode().needs_remote_account_provider() {
            Ok(Some(
                Self::new(
                    rpc_client,
                    pubsub_client,
                    subscription_forwarder,
                    config,
                    lrucache_subscribed_accounts,
                    chain_slot,
                )
                .await?,
            ))
        } else {
            Ok(None)
        }
    }

    /// Creates a background task that periodically reconciles subscriptions
    /// with the LRU (repairing missing ones, e.g. after a partial
    /// resubscription) and optionally updates the active subscriptions gauge
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_active_subscriptions_updater<
        PubsubClient: ChainPubsubClient,
    >(
        subscribed_accounts: Arc<AccountsLruCache>,
        secondary_subscriptions: Arc<AccountsLruCache>,
        confirmed_missing_subscriptions: Arc<Mutex<HashSet<Pubkey>>>,
        pubsub_client: Arc<PubsubClient>,
        removed_account_tx: mpsc::Sender<Pubkey>,
        subscription_key_locks: SubscriptionKeyLocks,
        subscription_ownership: SubscriptionOwnershipMap,
        fetching_accounts: Arc<FetchingAccounts>,
        capacity_eviction_protection: SharedCapacityEvictionProtectionPredicate,
        emit_metrics: bool,
    ) -> task::JoinHandle<()> {
        task::spawn(async move {
            let mut interval = time::interval(Duration::from_millis(
                ACTIVE_SUBSCRIPTIONS_UPDATE_INTERVAL_MS,
            ));
            let never_evicted = subscribed_accounts.never_evicted_accounts();

            loop {
                interval.tick().await;
                let pubsub_total =
                    subscription_reconciler::reconcile_subscriptions(
                        &subscribed_accounts,
                        &secondary_subscriptions,
                        &confirmed_missing_subscriptions,
                        pubsub_client.as_ref(),
                        &never_evicted,
                        &removed_account_tx,
                        Some(&subscription_key_locks),
                        Some(&subscription_ownership),
                        Some(fetching_accounts.as_ref()),
                        Some(&capacity_eviction_protection),
                    )
                    .await;

                debug!(count = pubsub_total, "Updating active subscriptions");
                if emit_metrics {
                    set_monitored_accounts_count(pubsub_total);
                }
            }
        })
    }

    /// Creates a new instance of the remote account provider
    /// By the time this method returns the current chain slot was resolved and
    /// a subscription setup to keep it up to date.
    pub(crate) async fn new(
        rpc_client: T,
        pubsub_client: U,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
        lrucache_subscribed_accounts: Arc<AccountsLruCache>,
        chain_slot: ChainSlot,
    ) -> RemoteAccountProviderResult<Self> {
        let secondary_subscriptions = Arc::new(AccountsLruCache::new(
            // SAFETY: config guarantees a non-zero capacity
            NonZeroUsize::new(config.secondary_subscriptions_lru_capacity())
                .expect("lru capacity must be non-zero"),
        ));
        Self::new_with_secondary_subscriptions(
            rpc_client,
            pubsub_client,
            subscription_forwarder,
            config,
            lrucache_subscribed_accounts,
            secondary_subscriptions,
            chain_slot,
        )
        .await
    }

    pub(super) async fn new_with_secondary_subscriptions(
        rpc_client: T,
        pubsub_client: U,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
        lrucache_subscribed_accounts: Arc<AccountsLruCache>,
        secondary_subscriptions: Arc<AccountsLruCache>,
        chain_slot: ChainSlot,
    ) -> RemoteAccountProviderResult<Self> {
        let (removed_account_tx, removed_account_rx) =
            tokio::sync::mpsc::channel(100);
        let subscription_key_locks: SubscriptionKeyLocks =
            Arc::new(AsyncMutex::new(HashMap::new()));
        let confirmed_missing_subscriptions =
            Arc::new(Mutex::new(HashSet::new()));
        let subscription_ownership: SubscriptionOwnershipMap =
            Arc::new(AsyncMutex::new(HashMap::new()));
        let fetching_accounts = Arc::<FetchingAccounts>::default();
        let capacity_eviction_protection:
            SharedCapacityEvictionProtectionPredicate =
            Arc::new(RwLock::new(None));

        // The reconciler always runs: partial resubscriptions rely on it for
        // repair. The config flag only gates the metrics emission.
        let active_subscriptions_updater =
            Some(Self::start_active_subscriptions_updater(
                lrucache_subscribed_accounts.clone(),
                secondary_subscriptions.clone(),
                confirmed_missing_subscriptions.clone(),
                Arc::new(pubsub_client.clone()),
                removed_account_tx.clone(),
                subscription_key_locks.clone(),
                subscription_ownership.clone(),
                fetching_accounts.clone(),
                capacity_eviction_protection.clone(),
                config.enable_subscription_metrics(),
            ));

        let me = Self {
            fetching_accounts,
            next_fetching_account_generation: AtomicU64::default(),
            subscription_ownership,
            subscription_transition_lock: Arc::new(AsyncMutex::new(())),
            subscription_key_locks,
            rpc_client,
            pubsub_client,
            chain_slot,
            last_update_slot: Arc::<AtomicU64>::default(),
            received_updates_count: Arc::<AtomicU64>::default(),
            lrucache_subscribed_accounts,
            secondary_subscriptions,
            confirmed_missing_subscriptions,
            capacity_eviction_protection,
            subscription_forwarder: Arc::new(subscription_forwarder),
            replay_outbox: Arc::default(),
            replay_notify: Arc::new(Notify::new()),
            removed_account_tx,
            removed_account_rx: Mutex::new(Some(removed_account_rx)),
            _active_subscriptions_task_handle: active_subscriptions_updater,
        };

        let updates = me.pubsub_client.take_updates();
        me.listen_for_account_updates(updates)?;
        me.start_replay_outbox_worker();
        let clock_remote_account = me
            .try_get(
                clock::ID,
                AccountFetchContext::internal(AccountFetchReason::Clock),
            )
            .await?;
        match clock_remote_account {
            RemoteAccount::NotFound(_) => {
                Err(RemoteAccountProviderError::ClockAccountCouldNotBeResolved(
                    clock::ID.to_string(),
                ))
            }
            RemoteAccount::Found(_) => {
                me.chain_slot.update(clock_remote_account.slot());
                Ok(me)
            }
        }
    }

    pub async fn try_new_from_endpoints(
        endpoints: &Endpoints,
        commitment: CommitmentConfig,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
        chain_slot: Arc<AtomicU64>,
    ) -> RemoteAccountProviderResult<
        RemoteAccountProvider<
            ChainRpcClientImpl,
            SubMuxClient<ChainUpdatesClient>,
        >,
    > {
        if endpoints.is_empty() {
            return Err(
                RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                    "No endpoints provided".to_string(),
                ),
            );
        }

        // Build RPC clients (use the first RPC endpoint found)
        let rpc_url = endpoints.rpc_url().ok_or_else(|| {
            RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                "No RPC endpoint found".to_string(),
            )
        })?;
        let rpc_client =
            ChainRpcClientImpl::new_from_url(rpc_url.as_str(), commitment);

        // Single-transport subscriptions: every client speaks the configured
        // transport. gRPC is the only supported production transport; with
        // it configured but unavailable, the validator logs an error and
        // runs degraded on websockets rather than refusing to start.
        let (grpc, ws): (Vec<_>, Vec<_>) = endpoints
            .pubsubs()
            .into_iter()
            .cloned()
            .partition(|ep| matches!(ep, Endpoint::Grpc { .. }));
        let grpc_selected = !grpc.is_empty();
        let selected = match config.subscription_transport() {
            SubscriptionTransport::Grpc if !grpc.is_empty() => {
                metrics::set_subscription_transport_grpc(true);
                grpc
            }
            SubscriptionTransport::Grpc => {
                error!(
                    "subscription-transport is grpc but no gRPC remote is \
                     configured; running DEGRADED on websockets — this is \
                     unsupported in production"
                );
                metrics::set_subscription_transport_grpc(false);
                ws.clone()
            }
            SubscriptionTransport::Ws => {
                warn!(
                    "subscription-transport is ws: dev/test only, \
                     unsupported in production"
                );
                metrics::set_subscription_transport_grpc(false);
                ws.clone()
            }
        };

        match Self::try_new_from_pubsubs(
            selected,
            commitment,
            rpc_client.clone(),
            chain_slot.clone(),
            subscription_forwarder.clone(),
            config,
        )
        .await
        {
            Ok(provider) => Ok(provider),
            Err(err)
                if matches!(
                    config.subscription_transport(),
                    SubscriptionTransport::Grpc
                ) && grpc_selected
                    && !ws.is_empty() =>
            {
                error!(
                    error = %err,
                    "gRPC subscriptions failed at startup; running DEGRADED \
                     on websockets — this is unsupported in production"
                );
                metrics::set_subscription_transport_grpc(false);
                Self::try_new_from_pubsubs(
                    ws,
                    commitment,
                    rpc_client,
                    chain_slot,
                    subscription_forwarder,
                    config,
                )
                .await
            }
            Err(err) => Err(err),
        }
    }

    pub(super) async fn try_new_from_pubsubs(
        pubsub_endpoints: Vec<Endpoint>,
        commitment: CommitmentConfig,
        rpc_client: ChainRpcClientImpl,
        chain_slot: Arc<AtomicU64>,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
    ) -> RemoteAccountProviderResult<
        RemoteAccountProvider<
            ChainRpcClientImpl,
            SubMuxClient<ChainUpdatesClient>,
        >,
    > {
        let resubscription_delay = config.resubscription_delay();
        let pubsub_futs = pubsub_endpoints.into_iter().map(|ep| {
            connect_pubsub_client(
                ep,
                commitment,
                rpc_client.clone(),
                chain_slot.clone(),
                resubscription_delay,
                config.grpc().clone(),
            )
        });
        let results = join_all(pubsub_futs).await;
        let pubsubs = collect_connected_pubsubs(results);

        if pubsubs.is_empty() {
            return Err(RemoteAccountProviderError::AllPubsubClientsFailed);
        }
        let subscribed_accounts = Arc::new(AccountsLruCache::new({
            // SAFETY: NonZeroUsize::new only returns None if the value is 0.
            // RemoteAccountProviderConfig can only be constructed with
            // capacity > 0
            let cap = config.subscribed_accounts_lru_capacity();
            NonZeroUsize::new(cap).expect("non-zero capacity")
        }));
        let secondary_subscriptions = Arc::new(AccountsLruCache::new({
            let cap = config.secondary_subscriptions_lru_capacity();
            NonZeroUsize::new(cap).expect("non-zero capacity")
        }));
        let subscribed_accounts_tracker =
            Arc::new(TieredSubscribedAccountsTracker::new(
                subscribed_accounts.clone(),
                secondary_subscriptions.clone(),
            ));

        let submux =
            SubMuxClient::new(pubsubs, subscribed_accounts_tracker, None);

        if !config.program_subs().is_empty() {
            let count = config.program_subs().len();
            debug!(count, "Subscribing to program accounts");
            let subscribe_program_futs = config
                .program_subs()
                .iter()
                .map(|program_id| submux.subscribe_program(*program_id));
            try_join_all(subscribe_program_futs).await?;
        }

        let provider = RemoteAccountProvider::<
            ChainRpcClientImpl,
            SubMuxClient<ChainUpdatesClient>,
        >::new_with_secondary_subscriptions(
            rpc_client,
            submux,
            subscription_forwarder,
            config,
            subscribed_accounts,
            secondary_subscriptions,
            ChainSlot::new(chain_slot),
        )
        .await?;
        Ok(provider)
    }

    pub(crate) fn promote_accounts(&self, pubkeys: &[&Pubkey]) {
        self.lrucache_subscribed_accounts.promote_multi(pubkeys);
        // This runs on the per-transaction ensure path; the secondary tier
        // only holds fetch-owned/missing accounts and is empty in the common
        // case, so skip its lock entirely then. A promote missed due to a
        // concurrent insert is harmless (LRU ordering is a heuristic).
        if !self.secondary_subscriptions.is_vacant() {
            self.secondary_subscriptions.promote_multi(pubkeys);
        }
    }

    pub(crate) async fn get_slot(&self) -> RemoteAccountProviderResult<u64> {
        tokio::time::timeout(RPC_FETCH_TIMEOUT, self.rpc_client.get_slot())
            .await
            .map_err(|_| {
                RemoteAccountProviderError::AccountResolutionsFailed(format!(
                    "RPC call timeout fetching slot after {}ms",
                    RPC_FETCH_TIMEOUT.as_millis()
                ))
            })?
            .map_err(|err| {
                RemoteAccountProviderError::AccountResolutionsFailed(format!(
                    "RpcError fetching slot: {err:?}"
                ))
            })
    }

    pub(crate) async fn get_program_accounts_with_config(
        &self,
        pubkey: &Pubkey,
        mut config: RpcProgramAccountsConfig,
    ) -> RemoteAccountProviderResult<Vec<(Pubkey, Account)>> {
        config.account_config.commitment = Some(self.rpc_client.commitment());

        tokio::time::timeout(RPC_FETCH_TIMEOUT, async {
            self.rpc_client
                .get_program_accounts_with_config(pubkey, config)
                .await
        })
        .await
        .map_err(|_| {
            RemoteAccountProviderError::AccountResolutionsFailed(format!(
                "RPC call timeout fetching program accounts for {pubkey} after {}ms",
                RPC_FETCH_TIMEOUT.as_millis()
            ))
        })?
        .map_err(|err| {
            RemoteAccountProviderError::AccountResolutionsFailed(format!(
                "RpcError fetching program accounts for {pubkey}: {err:?}"
            ))
        })
    }

    pub(crate) fn set_capacity_eviction_protection<F>(&self, predicate: F)
    where
        F: Fn(&Pubkey) -> CapacityEvictionProtection + Send + Sync + 'static,
    {
        let mut guard = self
            .capacity_eviction_protection
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        *guard = Some(Arc::new(predicate));
    }

    pub fn try_get_removed_account_rx(
        &self,
    ) -> RemoteAccountProviderResult<mpsc::Receiver<Pubkey>> {
        let mut rx = self
            .removed_account_rx
            .lock()
            .expect("removed_account_rx lock poisoned");
        rx.take().ok_or_else(|| {
            RemoteAccountProviderError::LruCacheRemoveAccountSenderSupportsSingleReceiverOnly
        })
    }

    pub fn chain_slot(&self) -> u64 {
        self.chain_slot.load()
    }

    pub fn last_update_slot(&self) -> u64 {
        self.last_update_slot.load(Ordering::Relaxed)
    }

    pub fn received_updates_count(&self) -> u64 {
        self.received_updates_count.load(Ordering::Relaxed)
    }
}
