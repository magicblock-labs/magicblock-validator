use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

pub(crate) use chain_pubsub_client::{
    ChainPubsubClient, ChainPubsubClientImpl,
};
pub(crate) use chain_rpc_client::{ChainRpcClient, ChainRpcClientImpl};
use config::RemoteAccountProviderConfig;
pub(crate) use errors::{
    RemoteAccountProviderError, RemoteAccountProviderResult,
};
use log::*;
use lru_cache::AccountsLruCache;
pub(crate) use remote_account::RemoteAccount;
pub use remote_account::RemoteAccountUpdateSource;
use solana_account::Account;
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_pubkey::Pubkey;
#[cfg(any(test, feature = "dev-context"))]
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::{
    client_error::ErrorKind, config::RpcAccountInfoConfig,
    custom_error::JSON_RPC_SERVER_ERROR_MIN_CONTEXT_SLOT_NOT_REACHED,
    request::RpcError,
};
use solana_sdk::{commitment_config::CommitmentConfig, sysvar::clock};
use tokio::{
    sync::{mpsc, oneshot},
    task,
    time::{self, Duration},
};

pub(crate) mod chain_pubsub_actor;
pub mod chain_pubsub_client;
pub mod chain_rpc_client;
pub mod config;
pub mod errors;
mod lru_cache;
pub mod program_account;
mod remote_account;

pub use chain_pubsub_actor::SubscriptionUpdate;
use magicblock_metrics::metrics::{
    inc_account_fetches_failed, inc_account_fetches_found,
    inc_account_fetches_not_found, inc_account_fetches_success,
    set_monitored_accounts_count,
};
pub use remote_account::{ResolvedAccount, ResolvedAccountSharedData};

use crate::{errors::ChainlinkResult, submux::SubMuxClient};

const ACTIVE_SUBSCRIPTIONS_UPDATE_INTERVAL_MS: u64 = 60_000;

// Maps pubkey -> (fetch_start_slot, requests_waiting)
type FetchResult = Result<RemoteAccount, RemoteAccountProviderError>;
type FetchingAccounts =
    Mutex<HashMap<Pubkey, (u64, Vec<oneshot::Sender<FetchResult>>)>>;

pub struct ForwardedSubscriptionUpdate {
    pub pubkey: Pubkey,
    pub account: RemoteAccount,
}

unsafe impl Send for ForwardedSubscriptionUpdate {}
unsafe impl Sync for ForwardedSubscriptionUpdate {}

// Not sure why helius uses a different code for this error
const HELIUS_CONTEXT_SLOT_NOT_REACHED: i64 = -32603;
pub struct RemoteAccountProvider<T: ChainRpcClient, U: ChainPubsubClient> {
    /// The RPC client to fetch accounts from chain the first time we receive
    /// a request for them
    rpc_client: T,
    /// The pubsub client to listen for updates on chain and keep the account
    /// states up to date
    pubsub_client: U,
    /// Minimal tracking of accounts currently being fetched to handle race conditions
    /// between fetch and subscription updates. Only used during active fetch operations.
    fetching_accounts: Arc<FetchingAccounts>,
    /// The current slot on chain, derived from the latest update of the clock
    /// account that we received
    chain_slot: Arc<AtomicU64>,

    /// The slot of the last account update we received
    last_update_slot: Arc<AtomicU64>,

    /// The total number of account updates we received
    received_updates_count: Arc<AtomicU64>,

    /// Tracks which accounts are currently subscribed to
    lrucache_subscribed_accounts: Arc<AccountsLruCache>,

    /// Channel to notify when an account is removed from the cache and thus no
    /// longer being watched
    removed_account_tx: mpsc::Sender<Pubkey>,
    /// Single listener channel sending an update when an account is removed
    /// and no longer being watched.
    removed_account_rx: Mutex<Option<mpsc::Receiver<Pubkey>>>,

    subscription_forwarder: Arc<mpsc::Sender<ForwardedSubscriptionUpdate>>,

    /// Task that periodically updates the active subscriptions gauge
    _active_subscriptions_task_handle: Option<task::JoinHandle<()>>,
}

// -----------------
// Configs
// -----------------
pub struct MatchSlotsConfig {
    pub max_retries: u64,
    pub retry_interval_ms: u64,
    pub min_context_slot: Option<u64>,
}

impl Default for MatchSlotsConfig {
    fn default() -> Self {
        Self {
            max_retries: 10,
            retry_interval_ms: 50,
            min_context_slot: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub rpc_url: String,
    pub pubsub_url: String,
}

impl
    RemoteAccountProvider<
        ChainRpcClientImpl,
        SubMuxClient<ChainPubsubClientImpl>,
    >
{
    pub async fn try_from_urls_and_config(
        endpoints: &[Endpoint],
        commitment: CommitmentConfig,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
    ) -> ChainlinkResult<
        Option<
            RemoteAccountProvider<
                ChainRpcClientImpl,
                SubMuxClient<ChainPubsubClientImpl>,
            >,
        >,
    > {
        let mode = config.lifecycle_mode();
        if mode.needs_remote_account_provider() {
            debug!(
                "Creating RemoteAccountProvider with {endpoints:?} and {commitment:?}",
            );
            Ok(Some(
                RemoteAccountProvider::<
                    ChainRpcClientImpl,
                    SubMuxClient<ChainPubsubClientImpl>,
                >::try_new_from_urls(
                    endpoints,
                    commitment,
                    subscription_forwarder,
                    config,
                )
                .await?,
            ))
        } else {
            Ok(None)
        }
    }
}

impl<T: ChainRpcClient, U: ChainPubsubClient> RemoteAccountProvider<T, U> {
    pub async fn try_from_clients_and_mode(
        rpc_client: T,
        pubsub_client: U,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
    ) -> ChainlinkResult<Option<RemoteAccountProvider<T, U>>> {
        if config.lifecycle_mode().needs_remote_account_provider() {
            Ok(Some(
                Self::new(
                    rpc_client,
                    pubsub_client,
                    subscription_forwarder,
                    config,
                )
                .await?,
            ))
        } else {
            Ok(None)
        }
    }

    /// Creates a background task that periodically updates the active subscriptions gauge
    fn start_active_subscriptions_updater<PubsubClient: ChainPubsubClient>(
        subscribed_accounts: Arc<AccountsLruCache>,
        pubsub_client: Arc<PubsubClient>,
    ) -> task::JoinHandle<()> {
        task::spawn(async move {
            let mut interval = time::interval(Duration::from_millis(
                ACTIVE_SUBSCRIPTIONS_UPDATE_INTERVAL_MS,
            ));
            let never_evicted = subscribed_accounts.never_evicted_accounts();

            loop {
                interval.tick().await;
                let lru_count = subscribed_accounts.len();
                let (pubsub_total, pubsub_without_never_evict) = pubsub_client
                    .subscription_count(Some(&never_evicted))
                    .await;

                let all_pubsub_subs = if log::log_enabled!(log::Level::Debug) {
                    pubsub_client.subscriptions()
                } else {
                    vec![]
                };
                if lru_count != pubsub_without_never_evict {
                    warn!(
                        "User account subscription counts LRU cache={} pubsub client={} don't match",
                        lru_count, pubsub_without_never_evict
                    );
                    if log::log_enabled!(log::Level::Debug) {
                        // Log all pubsub subscriptions for debugging
                        trace!(
                            "All pubsub subscriptions: {:?}",
                            all_pubsub_subs
                        );

                        // Find extra keys in pubsub that are not in LRU cache
                        let lru_pubkeys = subscribed_accounts.pubkeys();
                        let pubsub_subs_without_never_evict: HashSet<_> =
                            all_pubsub_subs
                                .iter()
                                .filter(|pk| !never_evicted.contains(pk))
                                .copied()
                                .collect();
                        let lru_pubkeys_set: HashSet<_> =
                            lru_pubkeys.into_iter().collect();

                        let extra_in_pubsub: Vec<_> =
                            pubsub_subs_without_never_evict
                                .difference(&lru_pubkeys_set)
                                .cloned()
                                .collect();
                        let extra_in_lru: Vec<_> = lru_pubkeys_set
                            .difference(&pubsub_subs_without_never_evict)
                            .cloned()
                            .collect();

                        if !extra_in_pubsub.is_empty() {
                            debug!("Extra pubkeys in pubsub client not in LRU cache: {:?}", extra_in_pubsub);
                        }
                        if !extra_in_lru.is_empty() {
                            debug!("Extra pubkeys in LRU cache not in pubsub client: {:?}", extra_in_lru);
                        }
                    }
                }

                debug!("Updating active subscriptions: count={}", pubsub_total);
                trace!("All subscriptions: {}", pubkeys_str(&all_pubsub_subs));
                set_monitored_accounts_count(pubsub_total);
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
    ) -> RemoteAccountProviderResult<Self> {
        let (removed_account_tx, removed_account_rx) =
            tokio::sync::mpsc::channel(100);
        let subscribed_accounts = Arc::new(AccountsLruCache::new({
            // SAFETY: NonZeroUsize::new only returns None if the value is 0.
            // RemoteAccountProviderConfig can only be constructed with
            // capacity > 0
            let cap = config.subscribed_accounts_lru_capacity();
            NonZeroUsize::new(cap).expect("non-zero capacity")
        }));

        let active_subscriptions_updater =
            if config.enable_subscription_metrics() {
                Some(Self::start_active_subscriptions_updater(
                    subscribed_accounts.clone(),
                    Arc::new(pubsub_client.clone()),
                ))
            } else {
                None
            };

        let me = Self {
            fetching_accounts: Arc::<FetchingAccounts>::default(),
            rpc_client,
            pubsub_client,
            chain_slot: Arc::<AtomicU64>::default(),
            last_update_slot: Arc::<AtomicU64>::default(),
            received_updates_count: Arc::<AtomicU64>::default(),
            lrucache_subscribed_accounts: subscribed_accounts.clone(),
            subscription_forwarder: Arc::new(subscription_forwarder),
            removed_account_tx,
            removed_account_rx: Mutex::new(Some(removed_account_rx)),
            _active_subscriptions_task_handle: active_subscriptions_updater,
        };

        let updates = me.pubsub_client.take_updates();
        me.listen_for_account_updates(updates)?;
        let clock_remote_account = me.try_get(clock::ID).await?;
        match clock_remote_account {
            RemoteAccount::NotFound(_) => {
                Err(RemoteAccountProviderError::ClockAccountCouldNotBeResolved(
                    clock::ID.to_string(),
                ))
            }
            RemoteAccount::Found(_) => {
                me.chain_slot
                    .store(clock_remote_account.slot(), Ordering::Relaxed);
                Ok(me)
            }
        }
    }

    pub async fn try_new_from_urls(
        endpoints: &[Endpoint],
        commitment: CommitmentConfig,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
    ) -> RemoteAccountProviderResult<
        RemoteAccountProvider<
            ChainRpcClientImpl,
            SubMuxClient<ChainPubsubClientImpl>,
        >,
    > {
        if endpoints.is_empty() {
            return Err(
                RemoteAccountProviderError::AccountSubscriptionsFailed(
                    "No endpoints provided".to_string(),
                ),
            );
        }

        // Build RPC clients (use the first one for now)
        let rpc_client = {
            let first = &endpoints[0];
            ChainRpcClientImpl::new_from_url(first.rpc_url.as_str(), commitment)
        };

        // Build pubsub clients and wrap them into a SubMuxClient
        let mut pubsubs: Vec<(Arc<ChainPubsubClientImpl>, mpsc::Receiver<()>)> =
            Vec::with_capacity(endpoints.len());
        for ep in endpoints {
            let (abort_tx, abort_rx) = mpsc::channel(1);
            let client = ChainPubsubClientImpl::try_new_from_url(
                ep.pubsub_url.as_str(),
                abort_tx,
                commitment,
            )
            .await?;
            pubsubs.push((Arc::new(client), abort_rx));
        }
        let submux = SubMuxClient::new(pubsubs, None);

        RemoteAccountProvider::<
            ChainRpcClientImpl,
            SubMuxClient<ChainPubsubClientImpl>,
        >::new(rpc_client, submux, subscription_forwarder, config)
        .await
    }

    pub(crate) fn promote_accounts(&self, pubkeys: &[&Pubkey]) {
        self.lrucache_subscribed_accounts.promote_multi(pubkeys);
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
        self.chain_slot.load(Ordering::Relaxed)
    }

    pub fn last_update_slot(&self) -> u64 {
        self.last_update_slot.load(Ordering::Relaxed)
    }

    pub fn received_updates_count(&self) -> u64 {
        self.received_updates_count.load(Ordering::Relaxed)
    }

    fn listen_for_account_updates(
        &self,
        mut updates: mpsc::Receiver<SubscriptionUpdate>,
    ) -> RemoteAccountProviderResult<()> {
        let fetching_accounts = self.fetching_accounts.clone();
        let chain_slot = self.chain_slot.clone();
        let received_updates_count = self.received_updates_count.clone();
        let last_update_slot = self.last_update_slot.clone();
        let subscription_forwarder = self.subscription_forwarder.clone();
        task::spawn(async move {
            while let Some(update) = updates.recv().await {
                let slot = update.rpc_response.context.slot;

                received_updates_count.fetch_add(1, Ordering::Relaxed);
                last_update_slot.store(slot, Ordering::Relaxed);

                if update.pubkey == clock::ID {
                    // We show as part of test_chain_pubsub_client_clock that the response
                    // context slot always matches the slot encoded in the slot data
                    chain_slot.store(slot, Ordering::Relaxed);
                    // NOTE: we do not forward clock updates
                } else {
                    trace!(
                        "Received account update for {} at slot {}",
                        update.pubkey,
                        slot
                    );
                    let remote_account =
                        match update.rpc_response.value.decode::<Account>() {
                            Some(account) => RemoteAccount::from_fresh_account(
                                account,
                                slot,
                                RemoteAccountUpdateSource::Subscription,
                            ),
                            None => {
                                error!(
                                "Account for {} update could not be decoded",
                                update.pubkey
                            );
                                RemoteAccount::NotFound(slot)
                            }
                        };

                    // Check if we're currently fetching this account
                    let forward_update = {
                        let mut fetching = fetching_accounts.lock().unwrap();
                        if let Some((fetch_start_slot, pending_requests)) =
                            fetching.remove(&update.pubkey)
                        {
                            // If subscription update is newer than when we started fetching,
                            // resolve with the subscription data instead
                            if slot >= fetch_start_slot {
                                trace!("Using subscription update for {} (slot {}) instead of fetch (started at slot {})",
                                    update.pubkey, slot, fetch_start_slot);

                                // Resolve all pending requests with subscription data
                                for sender in pending_requests {
                                    let _ =
                                        sender.send(Ok(remote_account.clone()));
                                }
                                None
                            } else {
                                // Subscription is stale, put the fetch tracking back
                                warn!("Received stale subscription update for {} at slot {}. Fetch started at slot {}",
                                    update.pubkey, slot, fetch_start_slot);
                                fetching.insert(
                                    update.pubkey,
                                    (fetch_start_slot, pending_requests),
                                );
                                None
                            }
                        } else {
                            Some(ForwardedSubscriptionUpdate {
                                pubkey: update.pubkey,
                                account: remote_account,
                            })
                        }
                    };

                    if let Some(forward_update) = forward_update {
                        if let Err(err) =
                            subscription_forwarder.send(forward_update).await
                        {
                            error!(
                                "Failed to forward subscription update for {}: {err:?}",
                                update.pubkey
                            );
                        }
                    }
                }
            }
        });
        Ok(())
    }

    /// Convenience wrapper around [`RemoteAccountProvider::try_get_multi`] to fetch
    /// a single account.
    pub async fn try_get(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<RemoteAccount> {
        self.try_get_multi(&[pubkey], None)
            .await
            // SAFETY: we are guaranteed to have a single result here as
            // otherwise we would have gotten an error
            .map(|mut accs| accs.drain(..).next().unwrap())
    }

    pub async fn try_get_multi_until_slots_match(
        &self,
        pubkeys: &[Pubkey],
        config: Option<MatchSlotsConfig>,
    ) -> RemoteAccountProviderResult<Vec<RemoteAccount>> {
        use SlotsMatchResult::*;

        // 1. Fetch the _normal_ way and hope the slots match and if required
        //    the min_context_slot is met
        let remote_accounts = self.try_get_multi(pubkeys, None).await?;
        if let Match = slots_match_and_meet_min_context(
            &remote_accounts,
            config.as_ref().and_then(|c| c.min_context_slot),
        ) {
            return Ok(remote_accounts);
        }

        let config = config.unwrap_or_default();
        // 2. Force a re-fetch unless all the accounts are already pending which
        //    means someone else already requested a re-fetch for all of them
        let refetch = {
            let fetching = self.fetching_accounts.lock().unwrap();
            pubkeys.iter().any(|pk| !fetching.contains_key(pk))
        };
        if refetch {
            if log::log_enabled!(log::Level::Trace) {
                trace!(
                    "Triggering re-fetch for accounts [{}] at slot {}",
                    pubkeys_str(pubkeys),
                    self.chain_slot()
                );
            }
            self.fetch(pubkeys.to_vec(), None, self.chain_slot());
        }

        // 3. Wait for the slots to match
        let mut retries = 0;
        loop {
            if log::log_enabled!(log::Level::Trace) {
                let slots = account_slots(&remote_accounts);
                let pubkey_slots = pubkeys
                    .iter()
                    .zip(slots)
                    .map(|(pk, slot)| format!("{pk}:{slot}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                trace!(
                    "Retry({}) account fetch to sync non-matching slots [{}]",
                    retries + 1,
                    pubkey_slots
                );
            }
            let remote_accounts = self.try_get_multi(pubkeys, None).await?;
            let slots_match_result = slots_match_and_meet_min_context(
                &remote_accounts,
                config.min_context_slot,
            );
            if let Match = slots_match_result {
                return Ok(remote_accounts);
            }

            retries += 1;
            if retries == config.max_retries {
                let remote_accounts =
                    remote_accounts.into_iter().map(|a| a.slot()).collect();
                match slots_match_result {
                    // SAFETY: Match case is already handled and returns
                    Match => unreachable!("we would have returned above"),
                    Mismatch => {
                        return Err(
                            RemoteAccountProviderError::SlotsDidNotMatch(
                                pubkeys_str(pubkeys),
                                remote_accounts,
                            ),
                        );
                    }
                    MatchButBelowMinContextSlot(slot) => {
                        return Err(
                            RemoteAccountProviderError::MatchingSlotsNotSatisfyingMinContextSlot(
                            pubkeys_str(pubkeys),
                            remote_accounts,
                            slot)
                        );
                    }
                }
            }

            // If the slots don't match then wait for a bit and retry
            tokio::time::sleep(tokio::time::Duration::from_millis(
                config.retry_interval_ms,
            ))
            .await;
        }
    }

    /// Gets the accounts for the given pubkeys by fetching from RPC.
    /// Always fetches fresh data. FetchCloner handles request deduplication.
    /// Subscribes first to catch any updates that arrive during fetch.
    pub async fn try_get_multi(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
    ) -> RemoteAccountProviderResult<Vec<RemoteAccount>> {
        if pubkeys.is_empty() {
            return Ok(vec![]);
        }

        if log_enabled!(log::Level::Trace) {
            trace!("Fetching accounts: [{}]", pubkeys_str(pubkeys));
        }

        // Create channels for potential subscription updates to override fetch results
        let mut subscription_overrides = vec![];
        let fetch_start_slot = self.chain_slot.load(Ordering::Relaxed);

        {
            let mut fetching = self.fetching_accounts.lock().unwrap();
            for &pubkey in pubkeys {
                let (sender, receiver) = oneshot::channel();
                match fetching.entry(pubkey) {
                    Entry::Occupied(mut entry) => {
                        entry.get_mut().1.push(sender);
                    }
                    Entry::Vacant(entry) => {
                        entry.insert((fetch_start_slot, vec![sender]));
                    }
                }
                subscription_overrides.push((pubkey, receiver));
            }
        }

        // Setup subscriptions first (to catch updates during fetch)
        self.setup_subscriptions(&subscription_overrides).await?;

        // Start the fetch
        let min_context_slot = fetch_start_slot;
        self.fetch(pubkeys.to_vec(), mark_empty_if_not_found, min_context_slot);

        // Wait for all accounts to resolve (either from fetch or subscription override)
        let mut resolved_accounts = vec![];
        let mut errors = vec![];

        for (idx, (pubkey, receiver)) in
            subscription_overrides.into_iter().enumerate()
        {
            match receiver.await {
                Ok(result) => match result {
                    Ok(remote_account) => {
                        resolved_accounts.push(remote_account)
                    }
                    Err(err) => {
                        error!("Failed to fetch account {pubkey}: {err}");
                        errors.push((idx, err));
                    }
                },
                Err(err) => {
                    warn!("RemoteAccountProvider::try_get_multi - Unexpected RecvError while awaiting account {pubkey} at index {idx}: {err:?}. This should not happen with Result-based channels. Context: fetch_start_slot={fetch_start_slot}, min_context_slot={min_context_slot}, total_pubkeys={}",
                      pubkeys.len());
                    error!("Failed to resolve account {pubkey}: {err:?}");
                    errors.push((
                        idx,
                        RemoteAccountProviderError::RecvrError(err),
                    ));
                }
            }
        }

        if errors.is_empty() {
            assert_eq!(
                resolved_accounts.len(),
                pubkeys.len(),
                "BUG: resolved accounts and pubkeys length mismatch"
            );
            Ok(resolved_accounts)
        } else {
            Err(RemoteAccountProviderError::AccountResolutionsFailed(
                errors
                    .iter()
                    .map(|(idx, err)| {
                        let pubkey = pubkeys
                            .get(*idx)
                            .map(|pk| pk.to_string())
                            .unwrap_or_else(|| {
                                "BUG: could not match pubkey".to_string()
                            });
                        format!("{pubkey}: {err:?}")
                    })
                    .collect::<Vec<_>>()
                    .join(",\n"),
            ))
        }
    }

    async fn setup_subscriptions(
        &self,
        subscribe_and_fetch: &[(Pubkey, oneshot::Receiver<FetchResult>)],
    ) -> RemoteAccountProviderResult<()> {
        if log_enabled!(log::Level::Debug) {
            let pubkeys = subscribe_and_fetch
                .iter()
                .map(|(pk, _)| pk.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            trace!("Subscribing to accounts: {pubkeys}");
        }
        for (pubkey, _) in subscribe_and_fetch.iter() {
            // Register the subscription for the pubkey (handles LRU cache and eviction first)
            self.register_subscription(pubkey).await?;
        }
        Ok(())
    }

    /// Registers a new subscription for the given pubkey.
    async fn register_subscription(
        &self,
        pubkey: &Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        // If an account is evicted then we need to unsubscribe from it first
        // and then inform upstream that we are no longer tracking it
        if let Some(evicted) = self.lrucache_subscribed_accounts.add(*pubkey) {
            trace!("Evicting {pubkey}");

            // 1. Unsubscribe from the account directly (LRU has already removed it)
            if let Err(err) = self.pubsub_client.unsubscribe(evicted).await {
                warn!(
                    "Failed to unsubscribe from pubsub for evicted account {evicted}: {err:?}");
                // Rollback the LRU add since eviction failed
                self.lrucache_subscribed_accounts.remove(pubkey);
                return Err(err);
            }

            // 2. Inform upstream so it can remove it from the store
            self.send_removal_update(evicted).await?;
        }

        // 3. Subscribe to the new account (only after successful eviction handling)
        if let Err(err) = self.pubsub_client.subscribe(*pubkey).await {
            // Rollback the LRU add since subscription failed
            self.lrucache_subscribed_accounts.remove(pubkey);
            return Err(err);
        }

        Ok(())
    }

    async fn send_removal_update(
        &self,
        evicted: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        self.removed_account_tx.send(evicted).await.map_err(
            RemoteAccountProviderError::FailedToSendAccountRemovalUpdate,
        )?;
        Ok(())
    }

    /// Check if an account is currently being watched (subscribed to)
    /// This does not consider accounts like the clock sysvar that are watched as
    /// part of the provider's internal logic.
    pub fn is_watching(&self, pubkey: &Pubkey) -> bool {
        self.lrucache_subscribed_accounts.contains(pubkey)
    }

    /// Check if an account is currently pending (being fetched)
    pub fn is_pending(&self, pubkey: &Pubkey) -> bool {
        let fetching = self.fetching_accounts.lock().unwrap();
        fetching.contains_key(pubkey)
    }

    /// Subscribe to an account for updates
    pub async fn subscribe(
        &self,
        pubkey: &Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        if self.is_watching(pubkey) {
            return Ok(());
        }

        self.register_subscription(pubkey).await?;
        Ok(())
    }

    /// Unsubscribe from an account
    pub async fn unsubscribe(
        &self,
        pubkey: &Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        if !self.lrucache_subscribed_accounts.contains(pubkey) {
            warn!(
                "Tried to unsubscribe from account {} that was not subscribed in the LRU cache",
                pubkey
            );
            return Ok(());
        }

        match self.pubsub_client.unsubscribe(*pubkey).await {
            Ok(()) => {
                // Only remove from LRU cache after successful pubsub unsubscribe
                self.lrucache_subscribed_accounts.remove(pubkey);
                self.send_removal_update(*pubkey).await?;
            }
            Err(err) => {
                warn!(
                    "Failed to unsubscribe from pubsub for {pubkey}: {err:?}"
                );
                // Don't remove from LRU cache if pubsub unsubscribe failed
                // This ensures LRU cache and pubsub client stay in sync
            }
        }

        Ok(())
    }

    /// Tries to fetch the given accounts from RPC.
    /// NOTE: if we get an RPC error we just log it and give up since there is no
    ///       obvious way how to handle this even if we were to bubble the error up.
    /// Any action that depends on those accounts to be there will fail.
    /// NOTE: this is not used during subscription updates since we receive the data
    ///       as part of that update, thus we won't have stale data issues.
    fn fetch(
        &self,
        pubkeys: Vec<Pubkey>,
        mark_empty_if_not_found: Option<&[Pubkey]>,
        min_context_slot: u64,
    ) {
        const MAX_RETRIES: u64 = 10;

        let rpc_client = self.rpc_client.clone();
        let fetching_accounts = self.fetching_accounts.clone();
        let commitment = self.rpc_client.commitment();
        let mark_empty_if_not_found =
            mark_empty_if_not_found.unwrap_or(&[]).to_vec();
        tokio::spawn(async move {
            use RemoteAccount::*;

            // Helper to notify all pending requests of fetch failure
            let notify_error = |error_msg: &str| {
                let mut fetching = fetching_accounts.lock().unwrap();
                error!("{error_msg}");
                inc_account_fetches_failed(pubkeys.len() as u64);
                for pubkey in &pubkeys {
                    // Update metrics
                    // Remove pending requests and send error
                    if let Some((_, requests)) = fetching.remove(pubkey) {
                        for sender in requests {
                            let error = RemoteAccountProviderError::AccountResolutionsFailed(
                                format!("{}: {}", pubkey, error_msg)
                            );
                            let _ = sender.send(Err(error));
                        }
                    }
                }
            };

            let mut remaining_retries: u64 = MAX_RETRIES;

            if log_enabled!(log::Level::Trace) {
                trace!("Fetch ({})", pubkeys_str(&pubkeys));
            }

            macro_rules! retry {
                ($msg:expr) => {{
                    trace!($msg);
                    remaining_retries -= 1;
                    if remaining_retries <= 0 {
                        let err_msg = format!("Max retries {MAX_RETRIES} reached, giving up on fetching accounts: {pubkeys:?}");
                        notify_error(&err_msg);
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    continue;
                }};
            }
            let response = loop {
                // We provide the min_context slot in order to _force_ the RPC to update
                // its account cache. Otherwise we could just keep fetching the accounts
                // until the context slot is high enough.
                match rpc_client
                    .get_multiple_accounts_with_config(
                        &pubkeys,
                        RpcAccountInfoConfig {
                            commitment: Some(commitment),
                            min_context_slot: Some(min_context_slot),
                            encoding: Some(UiAccountEncoding::Base64Zstd),
                            data_slice: None,
                        },
                    )
                    .await
                {
                    Ok(res) => {
                        let slot = res.context.slot;
                        if slot < min_context_slot {
                            retry!("Response slot {slot} < {min_context_slot}. Retrying...");
                        } else {
                            break res;
                        }
                    }
                    Err(err) => match err.kind {
                        ErrorKind::RpcError(rpc_err) => {
                            match rpc_err {
                                RpcError::ForUser(ref rpc_user_err) => {
                                    // When an account is not present for the desired
                                    // min-context slot then we normally get the below
                                    // handled `RpcResponseError`, but may also get the
                                    // following error from the RPC.
                                    // See test::ixtest_existing_account_for_future_slot
                                    // ```
                                    // RpcError(
                                    //   ForUser(
                                    //       "AccountNotFound: \
                                    // pubkey=DaeruQ4SukTQaJA5muyv51MQZok7oaCAF8fAW19mbJv5: \
                                    //        RPC response error -32016: \
                                    //        Minimum context slot has not been reached; ",
                                    //   ),
                                    // )
                                    // ```
                                    retry!("Fetching accounts failed: {rpc_user_err:?}");
                                 }
                                RpcError::RpcResponseError {
                                    code,
                                    message,
                                    data,
                                } => {
                                    if code == JSON_RPC_SERVER_ERROR_MIN_CONTEXT_SLOT_NOT_REACHED || code == HELIUS_CONTEXT_SLOT_NOT_REACHED {
                                        retry!("Minimum context slot {min_context_slot} not reached for {commitment:?}.");
                                    } else {
                                        let err = RpcError::RpcResponseError {
                                            code,
                                            message,
                                            data,
                                        };
                                        let err_msg = format!(
                                            "RpcError fetching accounts {}: {err:?}", pubkeys_str(&pubkeys)
                                        );
                                        notify_error(&err_msg);
                                        return;
                                    }
                                }
                                err => {
                                    let err_msg = format!(
                                        "RpcError fetching accounts {}: {err:?}", pubkeys_str(&pubkeys)
                                    );
                                     notify_error(&err_msg);
                                     return;
                                 }
                            }
                        }
                        _ => {
                            let err_msg = format!(
                                "RpcError fetching accounts {}: {err:?}",
                                pubkeys_str(&pubkeys)
                            );
                            notify_error(&err_msg);
                            return;
                        }
                    },
                };
            };

            // TODO: should we retry if not or respond with an error?
            assert!(response.context.slot >= min_context_slot);

            let mut found_count = 0u64;
            let mut not_found_count = 0u64;

            let remote_accounts: Vec<RemoteAccount> = pubkeys
                .iter()
                .zip(response.value)
                .map(|(pubkey, acc)| match acc {
                    Some(value) => {
                        found_count += 1;
                        RemoteAccount::from_fresh_account(
                            value,
                            response.context.slot,
                            RemoteAccountUpdateSource::Fetch,
                        )
                    }
                    None if mark_empty_if_not_found.contains(pubkey) => {
                        found_count += 1;
                        RemoteAccount::from_fresh_account(
                            Account {
                                lamports: 0,
                                data: vec![],
                                owner: Pubkey::default(),
                                executable: false,
                                rent_epoch: 0,
                            },
                            response.context.slot,
                            RemoteAccountUpdateSource::Fetch,
                        )
                    }
                    None => {
                        not_found_count += 1;
                        NotFound(response.context.slot)
                    }
                })
                .collect();

            // Update metrics for successful RPC fetch
            inc_account_fetches_success(pubkeys.len() as u64);
            inc_account_fetches_found(found_count);
            inc_account_fetches_not_found(not_found_count);

            if log_enabled!(log::Level::Trace) {
                let pubkeys = pubkeys
                    .iter()
                    .map(|pk| pk.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                trace!(
                    "Fetched({pubkeys}) {remote_accounts:?}, notifying pending requests"
                );
            }

            // Notify all pending requests with fetch results (unless subscription override occurred)
            for (pubkey, remote_account) in
                pubkeys.iter().zip(remote_accounts.iter())
            {
                let requests = {
                    let mut fetching = fetching_accounts.lock().unwrap();
                    // Remove from fetching and get pending requests
                    // Note: the account might have been resolved by subscription update already
                    if let Some((_, requests)) = fetching.remove(pubkey) {
                        requests
                    } else {
                        // Account was resolved by subscription update, skip
                        if log::log_enabled!(log::Level::Trace) {
                            trace!(
                                "Account {pubkey} was already resolved by subscription update"
                            );
                        }
                        continue;
                    }
                };

                // Send the fetch result to all waiting requests
                for request in requests {
                    let _ = request.send(Ok(remote_account.clone()));
                }
            }
        });
    }
}

impl RemoteAccountProvider<ChainRpcClientImpl, ChainPubsubClientImpl> {
    #[cfg(any(test, feature = "dev-context"))]
    pub fn rpc_client(&self) -> &RpcClient {
        &self.rpc_client.rpc_client
    }
}

impl
    RemoteAccountProvider<
        ChainRpcClientImpl,
        SubMuxClient<ChainPubsubClientImpl>,
    >
{
    #[cfg(any(test, feature = "dev-context"))]
    pub fn rpc_client(&self) -> &RpcClient {
        &self.rpc_client.rpc_client
    }
}

fn all_slots_match(accs: &[RemoteAccount]) -> bool {
    if accs.is_empty() {
        return true;
    }
    let slot = accs.first().unwrap().slot();
    accs.iter().all(|acc| acc.slot() == slot)
}

enum SlotsMatchResult {
    Match,
    Mismatch,
    MatchButBelowMinContextSlot(u64),
}

fn slots_match_and_meet_min_context(
    accs: &[RemoteAccount],
    min_context_slot: Option<u64>,
) -> SlotsMatchResult {
    if !all_slots_match(accs) {
        return SlotsMatchResult::Mismatch;
    }

    if let Some(min_slot) = min_context_slot {
        let respect_slot = accs
            .first()
            .is_none_or(|first_acc| first_acc.slot() >= min_slot);
        if respect_slot {
            SlotsMatchResult::Match
        } else {
            SlotsMatchResult::MatchButBelowMinContextSlot(min_slot)
        }
    } else {
        SlotsMatchResult::Match
    }
}

fn account_slots(accs: &[RemoteAccount]) -> Vec<u64> {
    accs.iter().map(|acc| acc.slot()).collect()
}

fn pubkeys_str(pubkeys: &[Pubkey]) -> String {
    pubkeys
        .iter()
        .map(|pk| pk.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod test {
    use solana_system_interface::program as system_program;

    use super::{chain_pubsub_client::mock::ChainPubsubClientMock, *};
    use crate::{
        config::LifecycleMode,
        testing::{
            init_logger,
            rpc_client_mock::{
                AccountAtSlot, ChainRpcClientMock, ChainRpcClientMockBuilder,
            },
            utils::random_pubkey,
        },
    };

    #[tokio::test]
    async fn test_get_non_existing_account() {
        init_logger();

        let remote_account_provider = {
            let (tx, rx) = mpsc::channel(1);
            let rpc_client = ChainRpcClientMockBuilder::new()
                .clock_sysvar_for_slot(1)
                .build();
            let pubsub_client =
                chain_pubsub_client::mock::ChainPubsubClientMock::new(tx, rx);
            let (fwd_tx, _fwd_rx) = mpsc::channel(100);
            let config = RemoteAccountProviderConfig::try_new_with_metrics(
                1000,
                LifecycleMode::Ephemeral,
                false,
            )
            .unwrap();
            RemoteAccountProvider::new(
                rpc_client,
                pubsub_client,
                fwd_tx,
                &config,
            )
            .await
            .unwrap()
        };

        let pubkey = random_pubkey();
        let remote_account =
            remote_account_provider.try_get(pubkey).await.unwrap();
        assert!(!remote_account.is_found());
    }

    #[tokio::test]
    async fn test_get_existing_account_for_valid_slot() {
        init_logger();

        const CURRENT_SLOT: u64 = 42;
        let pubkey = random_pubkey();

        let (remote_account_provider, rpc_client) = {
            let rpc_client = ChainRpcClientMockBuilder::new()
                .account(
                    pubkey,
                    Account {
                        lamports: 555,
                        data: vec![],
                        owner: system_program::id(),
                        executable: false,
                        rent_epoch: 0,
                    },
                )
                .clock_sysvar_for_slot(CURRENT_SLOT)
                .slot(CURRENT_SLOT)
                .build();
            let (tx, rx) = mpsc::channel(1);
            let pubsub_client =
                chain_pubsub_client::mock::ChainPubsubClientMock::new(tx, rx);
            (
                {
                    let (fwd_tx, _fwd_rx) = mpsc::channel(100);
                    let config =
                        RemoteAccountProviderConfig::try_new_with_metrics(
                            1000,
                            LifecycleMode::Ephemeral,
                            false,
                        )
                        .unwrap();
                    RemoteAccountProvider::new(
                        rpc_client.clone(),
                        pubsub_client,
                        fwd_tx,
                        &config,
                    )
                    .await
                    .unwrap()
                },
                rpc_client,
            )
        };

        let remote_account =
            remote_account_provider.try_get(pubkey).await.unwrap();
        let AccountAtSlot { account, slot } =
            rpc_client.get_account_at_slot(&pubkey).unwrap();
        assert_eq!(
            remote_account,
            RemoteAccount::from_fresh_account(
                account,
                slot,
                RemoteAccountUpdateSource::Fetch,
            )
        );
    }

    struct TestSlotConfig {
        current_slot: u64,
        account1_slot: u64,
        account2_slot: u64,
    }

    async fn setup_matching_slots(
        config: TestSlotConfig,
        pubkey1: Pubkey,
        pubkey2: Pubkey,
    ) -> (
        RemoteAccountProvider<ChainRpcClientMock, ChainPubsubClientMock>,
        mpsc::Receiver<ForwardedSubscriptionUpdate>,
    ) {
        init_logger();

        let rpc_client = ChainRpcClientMockBuilder::new()
            .slot(config.current_slot)
            .account(
                pubkey1,
                Account {
                    lamports: 555,
                    data: vec![],
                    owner: system_program::id(),
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .account(
                pubkey2,
                Account {
                    lamports: 666,
                    data: vec![],
                    owner: system_program::id(),
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .account_override_slot(&pubkey1, config.account1_slot)
            .account_override_slot(&pubkey2, config.account2_slot)
            .build();
        let (tx, rx) = mpsc::channel(1);
        let pubsub_client = ChainPubsubClientMock::new(tx, rx);

        let (forward_tx, forward_rx) = mpsc::channel(100);
        let config = RemoteAccountProviderConfig::try_new_with_metrics(
            1000,
            LifecycleMode::Ephemeral,
            false,
        )
        .unwrap();
        (
            RemoteAccountProvider::new(
                rpc_client,
                pubsub_client,
                forward_tx,
                &config,
            )
            .await
            .unwrap(),
            forward_rx,
        )
    }

    #[tokio::test]
    async fn test_get_accounts_until_slots_match_finding_matching_slot() {
        const CURRENT_SLOT: u64 = 42;
        let pubkey1 = random_pubkey();
        let pubkey2 = random_pubkey();
        let (remote_account_provider, _) = setup_matching_slots(
            TestSlotConfig {
                current_slot: CURRENT_SLOT,
                account1_slot: CURRENT_SLOT,
                account2_slot: CURRENT_SLOT + 1,
            },
            pubkey1,
            pubkey2,
        )
        .await;

        let remote_accounts = remote_account_provider
            .try_get_multi_until_slots_match(
                &[pubkey1, pubkey2],
                Some(MatchSlotsConfig {
                    max_retries: 10,
                    retry_interval_ms: 50,
                    min_context_slot: None,
                }),
            )
            .await
            .unwrap();

        assert_eq!(remote_accounts.len(), 2);
        assert!(remote_accounts[0].is_found());
        assert!(remote_accounts[1].is_found());
        assert_eq!(remote_accounts[0].fresh_lamports(), Some(555));
        assert_eq!(remote_accounts[1].fresh_lamports(), Some(666));
    }

    #[tokio::test]
    async fn test_get_accounts_until_slots_match_not_finding_matching_slot() {
        const CURRENT_SLOT: u64 = 42;
        let pubkey1 = random_pubkey();
        let pubkey2 = random_pubkey();
        let (remote_account_provider, _) = setup_matching_slots(
            TestSlotConfig {
                current_slot: CURRENT_SLOT,
                account1_slot: CURRENT_SLOT,
                account2_slot: CURRENT_SLOT - 1,
            },
            pubkey1,
            pubkey2,
        )
        .await;

        let res = remote_account_provider
            .try_get_multi_until_slots_match(
                &[pubkey1, pubkey2],
                Some(MatchSlotsConfig {
                    max_retries: 10,
                    retry_interval_ms: 50,
                    min_context_slot: None,
                }),
            )
            .await;

        debug!("Result: {res:?}");
        assert!(res.is_ok());
        let accs = res.unwrap();

        assert_eq!(accs.len(), 2);
        assert!(accs[0].is_found());
        assert!(!accs[1].is_found());
    }

    #[tokio::test]
    async fn test_get_accounts_until_slots_match_finding_matching_slot_but_chain_slot_smaller_than_min_context_slot(
    ) {
        const CURRENT_SLOT: u64 = 42;
        let pubkey1 = random_pubkey();
        let pubkey2 = random_pubkey();
        let (remote_account_provider, _) = setup_matching_slots(
            TestSlotConfig {
                current_slot: CURRENT_SLOT,
                account1_slot: CURRENT_SLOT,
                account2_slot: CURRENT_SLOT,
            },
            pubkey1,
            pubkey2,
        )
        .await;

        let res = remote_account_provider
            .try_get_multi_until_slots_match(
                &[pubkey1, pubkey2],
                Some(MatchSlotsConfig {
                    max_retries: 10,
                    retry_interval_ms: 50,
                    min_context_slot: Some(CURRENT_SLOT + 1),
                }),
            )
            .await;

        debug!("Result: {res:?}");

        assert!(res.is_err());
        assert!(matches!(
            res.unwrap_err(),
            RemoteAccountProviderError::MatchingSlotsNotSatisfyingMinContextSlot(
                _pubkeys,
                _slots,
                slot
            ) if slot == CURRENT_SLOT + 1
        ));
    }

    #[tokio::test]
    async fn test_get_accounts_until_slots_match_finding_matching_slot_but_one_account_slot_smaller_than_min_context_slot(
    ) {
        const CURRENT_SLOT: u64 = 42;
        let pubkey1 = random_pubkey();
        let pubkey2 = random_pubkey();
        let (remote_account_provider, _) = setup_matching_slots(
            TestSlotConfig {
                current_slot: CURRENT_SLOT,
                account1_slot: CURRENT_SLOT,
                account2_slot: CURRENT_SLOT - 1,
            },
            pubkey1,
            pubkey2,
        )
        .await;

        let res = remote_account_provider
            .try_get_multi_until_slots_match(
                &[pubkey1, pubkey2],
                Some(MatchSlotsConfig {
                    max_retries: 10,
                    retry_interval_ms: 50,
                    min_context_slot: Some(CURRENT_SLOT),
                }),
            )
            .await;

        debug!("Result: {res:?}");

        assert!(res.is_ok());
        let accs = res.unwrap();

        assert_eq!(accs.len(), 2);
        assert!(accs[0].is_found());
        assert!(!accs[1].is_found());
    }

    // -----------------
    // LRU Cache/Eviction/Removal
    // -----------------
    async fn setup_with_accounts(
        pubkeys: &[Pubkey],
        accounts_capacity: usize,
    ) -> (
        RemoteAccountProvider<ChainRpcClientMock, ChainPubsubClientMock>,
        mpsc::Receiver<ForwardedSubscriptionUpdate>,
        mpsc::Receiver<Pubkey>,
    ) {
        let rpc_client = {
            let mut rpc_client_builder =
                ChainRpcClientMockBuilder::new().slot(1);
            for pubkey in pubkeys {
                rpc_client_builder = rpc_client_builder.account(
                    *pubkey,
                    Account {
                        lamports: 555,
                        data: vec![],
                        owner: system_program::id(),
                        executable: false,
                        rent_epoch: 0,
                    },
                );
            }
            rpc_client_builder.build()
        };

        let (tx, rx) = mpsc::channel(1);
        let pubsub_client = ChainPubsubClientMock::new(tx, rx);

        let (forward_tx, forward_rx) = mpsc::channel(100);
        let provider = RemoteAccountProvider::new(
            rpc_client,
            pubsub_client,
            forward_tx,
            &RemoteAccountProviderConfig::try_new_with_metrics(
                accounts_capacity,
                LifecycleMode::Ephemeral,
                false,
            )
            .unwrap(),
        )
        .await
        .unwrap();

        let removed_account_tx = provider.try_get_removed_account_rx().unwrap();
        (provider, forward_rx, removed_account_tx)
    }

    fn drain_removed_account_rx(
        rx: &mut mpsc::Receiver<Pubkey>,
    ) -> Vec<Pubkey> {
        let mut removed_accounts = Vec::new();
        while let Ok(pubkey) = rx.try_recv() {
            removed_accounts.push(pubkey);
        }
        removed_accounts
    }

    #[tokio::test]
    async fn test_add_accounts_up_to_limit_no_eviction() {
        // Higher level version (including removed_rx) from
        // src/remote_account_provider/lru_cache.rs:
        // - test_lru_cache_add_accounts_up_to_limit_no_eviction
        init_logger();

        let pubkey1 = Pubkey::new_unique();
        let pubkey2 = Pubkey::new_unique();
        let pubkey3 = Pubkey::new_unique();

        let pubkeys = &[pubkey1, pubkey2, pubkey3];

        let (provider, _, mut removed_rx) =
            setup_with_accounts(pubkeys, 3).await;

        // Add three accounts (up to limit)
        for pk in pubkeys {
            provider.try_get(*pk).await.unwrap();
        }

        // No evictions should occur
        let removed = drain_removed_account_rx(&mut removed_rx);
        debug!("Removed accounts: {removed:?}");
        assert!(removed.is_empty(), "Expected no removed accounts");
    }

    #[tokio::test]
    async fn test_eviction_order() {
        // Higher level version (including removed_rx) from
        // src/remote_account_provider/lru_cache.rs:
        // - test_lru_cache_lru_eviction_order
        init_logger();

        let pubkey1 = Pubkey::new_unique();
        let pubkey2 = Pubkey::new_unique();
        let pubkey3 = Pubkey::new_unique();
        let pubkey4 = Pubkey::new_unique();
        let pubkey5 = Pubkey::new_unique();

        let pubkeys = &[pubkey1, pubkey2, pubkey3, pubkey4, pubkey5];
        let (provider, _, mut removed_rx) =
            setup_with_accounts(pubkeys, 3).await;

        // Fill cache: [1, 2, 3] (1 is least recently used)
        provider.try_get(pubkey1).await.unwrap();
        provider.try_get(pubkey2).await.unwrap();
        provider.try_get(pubkey3).await.unwrap();

        // Access pubkey1 to make it more recently used: [2, 3, 1]
        // This should just promote, making order [2, 3, 1]
        provider.try_get(pubkey1).await.unwrap();

        // Add pubkey4, should evict pubkey2 (now least recently used)
        provider.try_get(pubkey4).await.unwrap();

        // Check channel received the evicted account

        let removed_accounts = drain_removed_account_rx(&mut removed_rx);
        assert_eq!(removed_accounts, [pubkey2]);

        // Add pubkey5, should evict pubkey3 (now least recently used)
        provider.try_get(pubkey5).await.unwrap();

        // Check channel received the second evicted account
        let removed_accounts = drain_removed_account_rx(&mut removed_rx);
        assert_eq!(removed_accounts, [pubkey3]);
    }

    #[tokio::test]
    async fn test_multiple_evictions_in_sequence() {
        // Higher level version (including removed_rx) from
        // src/remote_account_provider/lru_cache.rs:
        // - test_lru_cache_multiple_evictions_in_sequence
        init_logger();

        // Create test pubkeys
        let pubkeys: Vec<Pubkey> =
            (1..=7).map(|_| Pubkey::new_unique()).collect();

        let (provider, _, mut removed_rx) =
            setup_with_accounts(&pubkeys, 4).await;

        // Fill cache to capacity (no evictions)
        for pk in pubkeys.iter().take(4) {
            provider.try_get(*pk).await.unwrap();
        }

        // Add more accounts and verify evictions happen in LRU order
        for i in 4..7 {
            provider.try_get(pubkeys[i]).await.unwrap();
            let expected_evicted = pubkeys[i - 4]; // Should evict the account added 4 steps ago

            // Verify the evicted account was sent over the channel
            let removed_accounts = drain_removed_account_rx(&mut removed_rx);
            assert_eq!(removed_accounts, vec![expected_evicted]);
        }
    }
}
