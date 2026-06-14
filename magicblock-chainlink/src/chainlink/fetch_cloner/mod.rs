use std::{
    collections::{hash_map, HashSet},
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use dlp_api::{
    pda::delegation_record_pda_from_delegated_account, state::DelegationRecord,
};
use lru::LruCache;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_aml::RiskService;
use magicblock_config::config::AllowedProgram;
use magicblock_core::token_programs::{
    try_derive_supported_ata_pubkeys, EATA_PROGRAM_ID,
};
use magicblock_metrics::metrics::{self, AccountFetchOrigin};
use parking_lot::Mutex as PlMutex;
use scc::HashMap;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use solana_signature::Signature;
use solana_signer::Signer;
use tokio::{
    sync::{mpsc, oneshot},
    task,
    task::JoinSet,
};
use tracing::*;

pub(crate) const FETCH_CLONE_OPERATION_TIMEOUT: Duration =
    Duration::from_secs(60);

mod ata_projection;
mod delegation;
mod pending_clone_guard;
mod pending_operation;
mod pipeline;
mod program_loader;
mod subscription;
#[cfg(test)]
mod tests;
mod types;

pub use self::types::FetchAndCloneResult;
use self::{
    pending_clone_guard::{
        CloneClaim, CloneCompletion, CloneKey, PendingCloneGuard,
    },
    pending_operation::{
        claim_or_join_pending, finish_pending, Pending, PendingClaim,
        PendingFailure, PendingHandles, PendingOwner, PendingTerminal,
        PendingWaiter,
    },
    subscription::{release_subs, SubscriptionRelease},
    types::{
        AccountWithCompanion, ClassifiedAccounts, PartitionedNotFound,
        RefreshDecision, ResolvedDelegatedAccounts, ResolvedPrograms,
    },
};
use super::{
    errors::{ChainlinkError, ChainlinkResult},
    should_schedule_bank_eviction,
};
use crate::{
    chainlink::{
        account_still_undelegating_on_chain::account_still_undelegating_on_chain,
        blacklisted_accounts::{
            blacklisted_accounts, programs_not_to_subscribe,
        },
    },
    cloner::{
        errors::{ClonerError, ClonerResult},
        AccountCloneRequest, Cloner, DelegationActions,
    },
    remote_account_provider::{
        program_account::get_loaderv3_get_program_data_address,
        pubsub_common::{is_internal_dlp_account_data, SubscriptionSource},
        CapacityEvictionProtection, ChainPubsubClient, ChainRpcClient,
        ForwardedSubscriptionUpdate, MatchSlotsConfig, RemoteAccount,
        RemoteAccountProvider, ResolvedAccountSharedData, SubscriptionReason,
    },
};

pub struct FetchCloner<T, U, V, C>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    /// The RemoteAccountProvider to fetch accounts from
    remote_account_provider: Arc<RemoteAccountProvider<T, U>>,
    /// Tracks pending account fetch requests to avoid duplicate fetches in parallel
    /// Once an account is fetched and cloned into the bank, it's removed from here
    pending_requests: Arc<HashMap<Pubkey, Pending>>,
    /// Monotonic generation for pending request ownership. Guards must match
    /// the stored generation before they can complete or clean up an entry.
    pending_request_generation: Arc<AtomicU64>,
    pending_waiter_generation: Arc<AtomicU64>,
    /// Counter to track the number of fetch operations for testing deduplication
    fetch_count: Arc<AtomicU64>,

    accounts_bank: Arc<V>,
    cloner: Arc<C>,
    validator_pubkey: Pubkey,
    validator_keypair: Arc<Keypair>,

    /// These are accounts that we should never clone into our validator.
    /// native programs, sysvars, native tokens, validator identity and faucet
    blacklisted_accounts: HashSet<Pubkey>,

    /// If specified, only these programs will be cloned. If None or empty,
    /// all programs are allowed.
    allowed_programs: Option<HashSet<Pubkey>>,

    /// Programs too broad for `subscribe_program`.
    programs_not_to_subscribe: HashSet<Pubkey>,

    /// Negative cache for derived eATAs confirmed missing on chain.
    known_empty_eatas: Arc<PlMutex<LruCache<Pubkey, ()>>>,

    /// Tracks in-flight clone operations per (pubkey, slot).
    /// The first caller to claim a key becomes the owner and performs
    /// the actual clone. Subsequent callers become waiters and receive
    /// the result via oneshot channels. Prevents duplicate clone
    /// submissions across concurrent fetch and subscription paths.
    pending_clones: Arc<
        Mutex<
            hash_map::HashMap<CloneKey, Vec<oneshot::Sender<CloneCompletion>>>,
        >,
    >,

    pending_operation_timeout_ms: Arc<AtomicU64>,

    /// Risk checker for post-delegation action addresses.
    risk_service: Option<Arc<RiskService>>,
}

/// Negative-cache capacity for known-empty eATAs.
const KNOWN_EMPTY_EATAS_CAPACITY: NonZeroUsize =
    match NonZeroUsize::new(100_000) {
        Some(n) => n,
        None => panic!("KNOWN_EMPTY_EATAS_CAPACITY must be non-zero"),
    };

/// Manual Clone impl: `#[derive(Clone)]` would add `V: Clone, C: Clone`
/// bounds that are not satisfied (`AccountsBank` and `Cloner` don't
/// require `Clone`). All fields are behind `Arc` so Clone is not needed.
impl<T, U, V, C> Clone for FetchCloner<T, U, V, C>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    fn clone(&self) -> Self {
        Self {
            remote_account_provider: self.remote_account_provider.clone(),
            pending_requests: self.pending_requests.clone(),
            pending_request_generation: self.pending_request_generation.clone(),
            pending_waiter_generation: self.pending_waiter_generation.clone(),
            fetch_count: self.fetch_count.clone(),
            accounts_bank: self.accounts_bank.clone(),
            cloner: self.cloner.clone(),
            validator_pubkey: self.validator_pubkey,
            validator_keypair: Arc::clone(&self.validator_keypair),
            blacklisted_accounts: self.blacklisted_accounts.clone(),
            allowed_programs: self.allowed_programs.clone(),
            programs_not_to_subscribe: self.programs_not_to_subscribe.clone(),
            known_empty_eatas: self.known_empty_eatas.clone(),
            pending_clones: self.pending_clones.clone(),
            pending_operation_timeout_ms: self
                .pending_operation_timeout_ms
                .clone(),
            risk_service: self.risk_service.clone(),
        }
    }
}

impl<T, U, V, C> FetchCloner<T, U, V, C>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    /// Create FetchCloner with subscription updates properly connected
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        remote_account_provider: &Arc<RemoteAccountProvider<T, U>>,
        accounts_bank: &Arc<V>,
        cloner: &Arc<C>,
        validator_keypair: Keypair,
        subscription_updates_rx: mpsc::Receiver<ForwardedSubscriptionUpdate>,
        allowed_programs: Option<Vec<AllowedProgram>>,
        risk_service: Option<Arc<RiskService>>,
    ) -> Arc<Self> {
        let validator_pubkey = validator_keypair.pubkey();
        let blacklisted_accounts = blacklisted_accounts(&validator_pubkey);
        let allowed_programs = allowed_programs.map(|programs| {
            programs.iter().map(|p| p.id).collect::<HashSet<_>>()
        });
        let me = Arc::new(Self {
            remote_account_provider: remote_account_provider.clone(),
            accounts_bank: accounts_bank.clone(),
            cloner: cloner.clone(),
            validator_pubkey,
            validator_keypair: Arc::new(validator_keypair),
            pending_requests: Arc::new(HashMap::new()),
            pending_request_generation: Arc::new(AtomicU64::new(1)),
            pending_waiter_generation: Arc::new(AtomicU64::new(1)),
            fetch_count: Arc::new(AtomicU64::new(0)),
            blacklisted_accounts,
            allowed_programs,
            programs_not_to_subscribe: programs_not_to_subscribe(),
            known_empty_eatas: Arc::new(PlMutex::new(LruCache::new(
                KNOWN_EMPTY_EATAS_CAPACITY,
            ))),
            pending_clones: Arc::new(Mutex::new(hash_map::HashMap::new())),
            pending_operation_timeout_ms: Arc::new(AtomicU64::new(
                FETCH_CLONE_OPERATION_TIMEOUT.as_millis() as u64,
            )),
            risk_service,
        });

        let accounts_bank_for_eviction = accounts_bank.clone();
        me.remote_account_provider.set_capacity_eviction_protection(
            move |pubkey| {
                accounts_bank_for_eviction
                    .get_account(pubkey)
                    .map(|account| CapacityEvictionProtection {
                        delegated: account.delegated(),
                        undelegating: account.undelegating(),
                        // Cloned base accounts are not ephemeral until after a successful evict
                        // so this won’t block legitimate cloned-account eviction
                        ephemeral: account.ephemeral()
                            && account.owner() != &Pubkey::default(),
                    })
                    .unwrap_or(CapacityEvictionProtection {
                        delegated: false,
                        undelegating: false,
                        ephemeral: false,
                    })
            },
        );

        me.clone()
            .start_subscription_listener(subscription_updates_rx);

        me
    }

    /// Get the current fetch count
    pub fn fetch_count(&self) -> u64 {
        self.fetch_count.load(Ordering::Relaxed)
    }

    #[instrument(skip(self, pubkeys))]
    pub async fn fetch_remote_accounts(
        &self,
        pubkeys: &[Pubkey],
        fetch_origin: AccountFetchOrigin,
    ) -> ChainlinkResult<Vec<RemoteAccount>> {
        Ok(self
            .remote_account_provider
            .try_get_multi(pubkeys, None, fetch_origin, None)
            .await?)
    }

    pub fn cloner(&self) -> &Arc<C> {
        &self.cloner
    }

    pub(crate) fn remote_account_provider(
        &self,
    ) -> &Arc<RemoteAccountProvider<T, U>> {
        &self.remote_account_provider
    }

    #[cfg(test)]
    fn has_pending_request(&self, pubkey: &Pubkey) -> bool {
        self.pending_requests.contains(pubkey)
    }

    #[cfg(test)]
    fn set_pending_operation_timeout(&self, timeout: Duration) {
        self.pending_operation_timeout_ms
            .store(timeout.as_millis() as u64, Ordering::Relaxed);
    }

    /// Returns the number of waiters currently registered for the pending
    /// fetch+clone request keyed by `pubkey`, or `None` if no pending
    /// request exists for that pubkey. Used by tests to deterministically
    /// observe waiter registration without relying on fixed sleeps.
    #[cfg(any(test, feature = "dev-context"))]
    pub fn pending_request_waiter_count(
        &self,
        pubkey: &Pubkey,
    ) -> Option<usize> {
        self.pending_requests
            .read(pubkey, |_, state| state.waiters.len())
    }

    /// Cancels the in-flight fetch+clone owner for `pubkey`, if one exists.
    pub fn cancel_pending(&self, pubkey: &Pubkey) {
        self.pending_requests
            .read(pubkey, |_, pending| pending.cancel.notify_one());
    }

    /// Cancels all in-flight fetch+clone owners.
    pub fn cancel_all_pending(&self) {
        self.pending_requests
            .scan(|_pubkey, pending| pending.cancel.notify_one());
    }

    /// Check if a program is allowed to be cloned.
    /// Returns true if:
    /// - No allowed_programs restriction is set (None), OR
    /// - The allowed_programs set is empty (treats empty as unrestricted), OR
    /// - The program is in the allowed_programs set
    fn is_program_allowed(&self, program_id: &Pubkey) -> bool {
        match &self.allowed_programs {
            None => true,
            Some(allowed) => {
                if allowed.is_empty() {
                    true
                } else {
                    allowed.contains(program_id)
                }
            }
        }
    }

    /// Attempt to claim ownership of a clone operation for (pubkey, slot).
    /// Returns `CloneClaim::Owner` if this caller is the first and should
    /// perform the clone. Returns `CloneClaim::Waiter(rx)` if another
    /// caller already owns this clone and this caller should wait.
    fn claim_pending_clone(&self, pubkey: Pubkey, slot: u64) -> CloneClaim {
        let key = (pubkey, slot);
        let mut map = self
            .pending_clones
            .lock()
            .expect("pending_clones mutex poisoned");
        match map.entry(key) {
            hash_map::Entry::Vacant(entry) => {
                entry.insert(Vec::new());
                CloneClaim::Owner
            }
            hash_map::Entry::Occupied(mut entry) => {
                let (tx, rx) = oneshot::channel();
                entry.get_mut().push(tx);
                CloneClaim::Waiter(rx)
            }
        }
    }

    /// Called by the owner when the clone operation completes.
    /// Removes the pending entry and notifies all waiters.
    fn finish_pending_clone(
        &self,
        pubkey: Pubkey,
        slot: u64,
        result: CloneCompletion,
    ) {
        let key = (pubkey, slot);
        let waiters = {
            let mut map = self
                .pending_clones
                .lock()
                .expect("pending_clones mutex poisoned");
            map.remove(&key).unwrap_or_default()
        };
        for tx in waiters {
            let _ = tx.send(result);
        }
    }

    fn claim_or_join_owned_operation(&self, pubkey: Pubkey) -> PendingClaim {
        let generation = self.next_pending_request_generation();
        let waiter_id = self.next_pending_waiter_id();
        claim_or_join_pending(
            self.pending_requests.clone(),
            pubkey,
            generation,
            waiter_id,
            Duration::from_millis(
                self.pending_operation_timeout_ms.load(Ordering::Relaxed),
            ),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_owned_operation(
        &self,
        pubkey: Pubkey,
        generation: u64,
        deadline: tokio::time::Instant,
        cancel: Arc<tokio::sync::Notify>,
        owner: PendingOwner,
        mark_empty_if_not_found: bool,
        slot: Option<u64>,
        fetch_origin: AccountFetchOrigin,
    ) {
        let this = self.clone();
        let pending = self.pending_requests.clone();
        task::spawn(async move {
            let mut owner = owner;
            let pubkeys = vec![pubkey];
            let mark_empty = mark_empty_if_not_found.then_some(vec![pubkey]);
            let mark_empty_ref = mark_empty.as_deref();
            let work = this.fetch_and_clone_accounts(
                &pubkeys,
                mark_empty_ref,
                slot,
                fetch_origin,
            );
            let terminal = tokio::select! {
                biased;

                result = tokio::time::timeout_at(deadline, work) => {
                    match result {
                        Ok(Ok(result)) => PendingTerminal::Success(result),
                        Ok(Err(err)) => PendingTerminal::Failed(
                            PendingFailure::OwnerFailed(err.to_string()),
                        ),
                        Err(_) => PendingTerminal::Failed(PendingFailure::TimedOut),
                    }
                }
                _ = cancel.notified() => {
                    PendingTerminal::Failed(PendingFailure::Cancelled)
                }
            };
            finish_pending(&pending, pubkey, generation, terminal);
            owner.dismiss();
        });
    }

    fn next_pending_request_generation(&self) -> u64 {
        self.pending_request_generation
            .fetch_add(1, Ordering::Relaxed)
    }

    fn next_pending_waiter_id(&self) -> u64 {
        self.pending_waiter_generation
            .fetch_add(1, Ordering::Relaxed)
    }

    /// Submits a clone request through ownership coordination.
    /// Only one caller per (pubkey, slot) will actually submit the
    /// clone transaction. All other concurrent callers wait for the
    /// owner's result.
    async fn clone_account_with_ownership(
        &self,
        request: AccountCloneRequest,
    ) -> ClonerResult<Signature> {
        let pubkey = request.pubkey;
        let slot = request.account.remote_slot();

        match self.claim_pending_clone(pubkey, slot) {
            CloneClaim::Owner => {
                let mut guard = PendingCloneGuard::new(
                    Arc::clone(&self.pending_clones),
                    pubkey,
                    slot,
                );
                let result = self.cloner.clone_account(request).await;
                let completion = if result.is_ok() {
                    CloneCompletion::Success
                } else {
                    CloneCompletion::Failed
                };
                self.finish_pending_clone(pubkey, slot, completion);
                guard.dismiss();
                result
            }
            CloneClaim::Waiter(rx) => match rx.await {
                Ok(CloneCompletion::Success) => Ok(Signature::default()),
                Ok(CloneCompletion::Failed) => {
                    Err(ClonerError::FailedToCloneRegularAccount(
                        pubkey,
                        Box::new(ClonerError::CommittorServiceError(
                            "Clone owner failed".to_string(),
                        )),
                    ))
                }
                Err(_) => Err(ClonerError::FailedToCloneRegularAccount(
                    pubkey,
                    Box::new(ClonerError::CommittorServiceError(
                        "Clone owner dropped".to_string(),
                    )),
                )),
            },
        }
    }

    async fn clone_account_with_post_delegation_action_invariants(
        &self,
        mut request: AccountCloneRequest,
    ) -> ChainlinkResult<Signature> {
        self.normalize_unresolved_dlp_clone_request(&mut request)?;

        if request.account.delegated()
            && self.local_delegated_clone_target_active(request.pubkey)
        {
            return Ok(Signature::default());
        }

        if request.delegation_actions.is_empty() {
            return Ok(self.clone_account_with_ownership(request).await?);
        }

        if !request.account.delegated() {
            return Err(ChainlinkError::InvalidDelegationActions(
                request.pubkey,
                "post-delegation actions attached to non-delegated clone target"
                    .to_string(),
            ));
        }

        self.ensure_delegation_action_dependencies(
            request.pubkey,
            request.account.remote_slot(),
            &request.delegation_actions,
        )
        .await?;

        Ok(self.clone_account_with_ownership(request).await?)
    }

    fn normalize_unresolved_dlp_clone_request(
        &self,
        request: &mut AccountCloneRequest,
    ) -> ChainlinkResult<()> {
        if request.account.owner() != &dlp_api::id()
            || !request.account.delegated()
        {
            return Ok(());
        }

        if request.pubkey
            == dlp_api::pda::magic_fee_vault_pda_from_validator(
                &self.validator_pubkey,
            )
        {
            return Ok(());
        }

        if !request.delegation_actions.is_empty() {
            return Err(ChainlinkError::InvalidDelegationActions(
                request.pubkey,
                "post-delegation actions attached to unresolved DLP-owned clone target"
                    .to_string(),
            ));
        }

        request.account.set_delegated(false);
        request.account.set_confined(false);
        Ok(())
    }

    fn local_delegated_clone_target_active(&self, pubkey: Pubkey) -> bool {
        self.accounts_bank
            .get_account(&pubkey)
            .is_some_and(|account| account.delegated())
    }

    /// Start listening to subscription updates.
    /// Uses a JoinSet-based loop with try_recv/select! for backpressure
    /// and task lifecycle management instead of unbounded tokio::spawn.
    pub fn start_subscription_listener(
        self: Arc<Self>,
        mut subscription_updates: mpsc::Receiver<ForwardedSubscriptionUpdate>,
    ) {
        tokio::spawn(async move {
            let mut pending_tasks: JoinSet<()> = JoinSet::new();

            loop {
                match subscription_updates.try_recv() {
                    Ok(update) => {
                        let pubkey = update.pubkey;
                        trace!(
                            pubkey = %pubkey,
                            "FetchCloner received subscription update"
                        );

                        let this = Arc::clone(&self);
                        pending_tasks.spawn(async move {
                            Self::process_subscription_update(
                                &this, pubkey, update,
                            )
                            .await;
                        });

                        while let Some(result) = pending_tasks.try_join_next() {
                            if let Err(err) = result {
                                warn!(
                                    error = ?err,
                                    "Subscription update task panicked"
                                );
                            }
                        }
                    }
                    Err(mpsc::error::TryRecvError::Empty) => {
                        tokio::select! {
                            maybe_update =
                                subscription_updates.recv() =>
                            {
                                let Some(update) = maybe_update else {
                                    while pending_tasks
                                        .join_next()
                                        .await
                                        .is_some()
                                    {}
                                    break;
                                };
                                let pubkey = update.pubkey;
                                let this = Arc::clone(&self);
                                pending_tasks.spawn(async move {
                                    Self::process_subscription_update(
                                        &this, pubkey, update,
                                    )
                                    .await;
                                });
                            }
                            Some(result) = pending_tasks.join_next(),
                                if !pending_tasks.is_empty() =>
                            {
                                if let Err(err) = result {
                                    error!(
                                        error = ?err,
                                        "Subscription update task panicked"
                                    );
                                }
                            }
                        }
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        while pending_tasks.join_next().await.is_some() {}
                        break;
                    }
                }
            }
        });
    }

    async fn process_subscription_update(
        &self,
        pubkey: Pubkey,
        update: ForwardedSubscriptionUpdate,
    ) {
        if self
            .maybe_greedily_clone_discovered_delegated_account(pubkey, &update)
            .await
        {
            return;
        }

        // A late forwarded update can arrive after an account was removed from
        // the provider watch set. If a new subscription already won the race,
        // is_watching is true and this update can be processed normally. If this
        // update wins before acquire_subscription completes, the update is dropped;
        // the new subscription path performs its own fetch and clones fresh state.
        // If stale state is still present locally, cleanup is routed through the
        // existing removal listener, which serializes the final is_watching check and
        // eviction submission against same-pubkey subscription transitions.
        //
        // The guard only applies to account-subscription updates: the
        // account-sub LRU is the source of truth for `is_watching`. Program
        // subscription updates can legitimately arrive for pubkeys that are
        // *not* in the account-sub LRU (e.g. delegated accounts whose direct
        // subscription was released after cloning and are now tracked only via
        // their owner program). Dropping those would leave the bank stuck in a
        // stale delegated/undelegated state.
        let update_slot = update.account.slot();
        if matches!(update.source, SubscriptionSource::Account)
            && !self.remote_account_provider.is_watching(&pubkey)
        {
            trace!(
                pubkey = %pubkey,
                update_slot,
                "Dropping subscription update for account that is no longer watched"
            );
            if self
                .accounts_bank
                .get_account(&pubkey)
                .is_some_and(|account| should_schedule_bank_eviction(&account))
            {
                if let Err(err) = self
                    .remote_account_provider
                    .send_removal_update(pubkey)
                    .await
                {
                    warn!(
                        pubkey = %pubkey,
                        error = ?err,
                        "Failed to enqueue stale subscription update removal"
                    );
                }
            }
            return;
        }

        let (resolved_account, deleg_record, delegation_actions) = self
            .resolve_account_to_clone_from_forwarded_sub_with_unsubscribe(
                update,
            )
            .await;
        let Some(account) = resolved_account else {
            return;
        };
        let projected_ata_clone_request = self
            .maybe_build_projected_ata_clone_request_from_subscription_update(
                pubkey,
                &account,
                deleg_record.as_ref(),
                &delegation_actions,
            )
            .await;

        //
        // Ensure that the subscription update isn't out of order, i.e.
        // we already hold a newer version of the account in our bank.
        //
        // The stricter intent is to ignore non-advancing subscription updates: if the bank
        // already has the account at the same slot, then a normal/plain update at that slot is
        // treated as stale/duplicate and should not overwrite local state, with the following
        // exception:
        //
        //  - In the undelegate/redelegate same-slot path, the bank can still hold a plain
        //    or undelegating version while the subscription update carries the delegated state
        //    at the same slot, so we must allow that update.
        //
        let non_advancing_slot =
            self.accounts_bank.get_account(&pubkey).and_then(|in_bank| {
                let bank_slot = in_bank.remote_slot();
                let update_slot = account.remote_slot();
                let same_slot_delegated_refresh = bank_slot == update_slot
                    && account.delegated()
                    && (!in_bank.delegated() || in_bank.undelegating());
                if bank_slot > update_slot
                    || (bank_slot == update_slot
                        && !same_slot_delegated_refresh)
                {
                    Some(bank_slot)
                } else {
                    None
                }
            });

        if let Some(in_bank_slot) = non_advancing_slot {
            let update_slot = account.remote_slot();
            if in_bank_slot == update_slot {
                if let Some(projected_ata_clone_request) =
                    projected_ata_clone_request
                {
                    if let Err(err) = self
                        .clone_projected_ata_request(
                            projected_ata_clone_request,
                        )
                        .await
                    {
                        warn!(
                            pubkey = %pubkey,
                            error = %err,
                            "Failed to clone projected ATA from out-of-order delegated eATA update"
                        );
                    }
                }
            }
            trace!(
                pubkey = %pubkey,
                bank_slot = in_bank_slot,
                update_slot,
                "Ignoring out-of-order subscription update"
            );
            return;
        }

        let mut undelegation_completed_on_chain = false;
        if let Some(in_bank) = self.accounts_bank.get_account(&pubkey) {
            if in_bank.delegated() && !in_bank.undelegating() {
                self.cleanup_direct_subscription_for_delegated_account(pubkey)
                    .await;
                return;
            }

            if in_bank.undelegating() {
                debug!(
                    pubkey = %pubkey,
                    in_bank_delegated = in_bank.delegated(),
                    in_bank_owner = %in_bank.owner(),
                    in_bank_slot = in_bank.remote_slot(),
                    chain_delegated = account.delegated(),
                    chain_owner = %account.owner(),
                    chain_slot = account.remote_slot(),
                    "Received update for undelegating account"
                );

                if account.delegated()
                    && ata_projection::derive_eata_pubkey_from_ata_account(
                        &pubkey, &account,
                    )
                    .is_some()
                    && deleg_record.as_ref().is_some_and(|record| {
                        record.owner == EATA_PROGRAM_ID
                            && record.authority == self.validator_pubkey
                    })
                {
                    debug!(
                        pubkey = %pubkey,
                        "Keeping undelegating ATA in bank while companion eATA remains delegated"
                    );
                    return;
                }

                // This will only be true in the following case:
                // 1. a commit was triggered for the account
                // 2. a commit + undelegate was triggered for the account -> undelegating
                // 3. we receive the update for (1.)
                //
                // Thus our state is more up to date and we don't
                // need to update our bank.
                if account_still_undelegating_on_chain(
                    &pubkey,
                    account.delegated(),
                    in_bank.remote_slot(),
                    deleg_record,
                    &self.validator_pubkey,
                ) {
                    return;
                }
                undelegation_completed_on_chain = true;
            } else if !in_bank.delegated() && account.delegated() {
                undelegation_completed_on_chain = true;
            } else if in_bank.owner().eq(&dlp_api::id()) {
                debug!(
                    pubkey = %pubkey,
                    "Received update for account owned by delegation program but not marked as undelegating"
                );
            }
        } else {
            debug!(
                pubkey = %pubkey,
                "Received update for account not in bank"
            );
            if account.delegated() {
                undelegation_completed_on_chain = true;
            }
        }

        // Determine if delegated to another validator
        let delegated_to_other = deleg_record
            .as_ref()
            .and_then(|dr| self.get_delegated_to_other(dr));

        // Delegated subscription cleanup is limited to direct subscription/LRU
        // ownership here; undelegation tracking owns protected subscriptions
        // until undelegation is explicitly complete.
        if undelegation_completed_on_chain {
            if !account.delegated() {
                self.ensure_direct_subscription_for_completed_account(pubkey)
                    .await;
            }
            self.cleanup_undelegation_tracking_for_completed_account(pubkey)
                .await;
        }
        if account.delegated() {
            self.cleanup_direct_subscription_for_delegated_account(pubkey)
                .await;
        }

        if account.executable() {
            self.handle_executable_sub_update(pubkey, account).await;
        } else {
            let commit_frequency_ms = deleg_record.as_ref().and_then(|dr| {
                dr.authority
                    .eq(&self.validator_pubkey)
                    .then_some(dr.commit_frequency_ms)
            });
            let raw_delegation_actions = if account.delegated()
                && projected_ata_clone_request.is_none()
            {
                delegation_actions
            } else {
                DelegationActions::default()
            };
            if let Err(err) = self
                .clone_account_with_post_delegation_action_invariants(
                    AccountCloneRequest {
                        pubkey,
                        account,
                        commit_frequency_ms,
                        delegation_actions: raw_delegation_actions,
                        delegated_to_other,
                    },
                )
                .await
            {
                error!(
                    pubkey = %pubkey,
                    error = %err,
                    "Failed to clone account into bank"
                );
            } else if let Some(projected_ata_clone_request) =
                projected_ata_clone_request
            {
                if let Err(err) = self
                    .clone_projected_ata_request(projected_ata_clone_request)
                    .await
                {
                    error!(
                        pubkey = %pubkey,
                        error = %err,
                        "Failed to clone projected ATA from delegated eATA update"
                    );
                }
            }
        }
    }

    async fn ensure_delegation_action_dependencies(
        &self,
        pubkey: Pubkey,
        remote_slot: u64,
        delegation_actions: &DelegationActions,
    ) -> ChainlinkResult<()> {
        if delegation_actions.is_empty() {
            return Ok(());
        }

        self.validate_post_delegation_action_signers(delegation_actions)
            .await?;

        let (dependencies, writable_dependencies) =
            Self::collect_post_delegation_action_dependencies(
                pubkey,
                delegation_actions,
            );

        let dependencies_to_fetch = dependencies
            .into_iter()
            .filter(|dependency| {
                self.delegation_action_dependency_needs_fetch(
                    dependency,
                    &writable_dependencies,
                )
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        if dependencies_to_fetch.is_empty() {
            return Ok(());
        }

        let result = self
            .fetch_and_clone_accounts_with_dedup_forced_refresh(
                &dependencies_to_fetch,
                None,
                Some(remote_slot),
                AccountFetchOrigin::GetAccount,
                &writable_dependencies,
            )
            .await?;
        if result.missing_delegation_record.is_empty() {
            return Ok(());
        }

        let missing_accounts = result
            .pubkeys_missing_delegation_record()
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut missing_accounts = missing_accounts;
        missing_accounts.sort_unstable();
        Err(ChainlinkError::MissingDelegationActionAccounts(
            missing_accounts,
        ))
    }

    fn collect_post_delegation_action_dependencies(
        target: Pubkey,
        delegation_actions: &DelegationActions,
    ) -> (HashSet<Pubkey>, HashSet<Pubkey>) {
        let mut dependencies = HashSet::new();
        let mut writable_dependencies = HashSet::new();
        for instruction in delegation_actions.iter() {
            if instruction.program_id != target {
                dependencies.insert(instruction.program_id);
            }
            for meta in &instruction.accounts {
                if meta.pubkey == target {
                    continue;
                }
                dependencies.insert(meta.pubkey);
                if meta.is_writable {
                    writable_dependencies.insert(meta.pubkey);
                }
            }
        }
        (dependencies, writable_dependencies)
    }

    fn delegation_action_dependency_needs_fetch(
        &self,
        dependency: &Pubkey,
        writable_dependencies: &HashSet<Pubkey>,
    ) -> bool {
        let Some(account) = self.accounts_bank.get_account(dependency) else {
            return true;
        };
        writable_dependencies.contains(dependency)
            && (!account.delegated() || account.undelegating())
    }

    async fn validate_post_delegation_action_signers(
        &self,
        delegation_actions: &DelegationActions,
    ) -> ChainlinkResult<()> {
        let Some(risk_service) = self.risk_service.as_ref() else {
            return Ok(());
        };

        let mut signers = delegation_actions
            .iter()
            .flat_map(|instruction| {
                instruction.accounts.iter().filter_map(|meta| {
                    if meta.is_signer {
                        Some(meta.pubkey.to_string())
                    } else {
                        None
                    }
                })
            })
            .collect::<Vec<_>>();
        signers.sort_unstable();
        signers.dedup();

        if signers.is_empty() {
            return Ok(());
        }
        Ok(risk_service.check_addresses(signers).await?)
    }

    async fn clone_projected_ata_request(
        &self,
        request: AccountCloneRequest,
    ) -> ChainlinkResult<Signature> {
        if self
            .accounts_bank
            .get_account(&request.pubkey)
            .is_some_and(|account| account.undelegating())
        {
            return Ok(Signature::default());
        }

        self.clone_account_with_post_delegation_action_invariants(request)
            .await
    }

    async fn maybe_greedily_clone_discovered_delegated_account(
        &self,
        pubkey: Pubkey,
        update: &ForwardedSubscriptionUpdate,
    ) -> bool {
        if self.accounts_bank.get_account(&pubkey).is_some() {
            return false;
        }

        let Some(account) = update.account.fresh_account() else {
            return false;
        };

        if !account.owner().eq(&dlp_api::id()) {
            return false;
        }

        let Some((deleg_record, delegation_actions)) = self
            .fetch_and_parse_delegation_record(
                pubkey,
                account.remote_slot(),
                AccountFetchOrigin::GetAccount,
            )
            .await
        else {
            trace!(
                pubkey = %pubkey,
                slot = account.remote_slot(),
                "Greedy discovery could not resolve delegation record; falling back"
            );
            return false;
        };

        let is_delegated_to_us = deleg_record.authority
            == self.validator_pubkey
            || deleg_record.authority == Pubkey::default();
        if !is_delegated_to_us {
            trace!(
                pubkey = %pubkey,
                authority = %deleg_record.authority,
                "Ignoring discovered DLP-owned update delegated elsewhere"
            );
            return true;
        }
        let delegation_actions = delegation_actions.unwrap_or_default();

        let greedy_ata_pubkeys = delegation::parse_raw_eata_pda(
            &pubkey,
            account.data(),
            deleg_record.owner,
        )
        .map(|(wallet_owner, mint)| {
            try_derive_supported_ata_pubkeys(&wallet_owner, &mint)
                .token_2022_first()
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
        let mut pubkeys_to_clone =
            Vec::with_capacity(1 + greedy_ata_pubkeys.len());
        pubkeys_to_clone.push(pubkey);
        pubkeys_to_clone.extend(greedy_ata_pubkeys.iter().copied().filter(
            |ata_pubkey| self.accounts_bank.get_account(ata_pubkey).is_none(),
        ));

        // Keep eATA discovery with its candidate base ATAs in one clone batch
        // so the normal ATA projection path runs for the same update.
        let clone_result = if greedy_ata_pubkeys.is_empty() {
            self.fetch_and_clone_accounts_with_dedup(
                &pubkeys_to_clone,
                None,
                Some(account.remote_slot()),
                AccountFetchOrigin::GetAccount,
            )
            .await
        } else {
            self.fetch_and_clone_accounts(
                &pubkeys_to_clone,
                None,
                Some(account.remote_slot()),
                AccountFetchOrigin::GetAccount,
            )
            .await
        };

        match clone_result {
            Ok(result)
                if result
                    .not_found_on_chain
                    .iter()
                    .all(|(missing_pubkey, _)| missing_pubkey != &pubkey)
                    && result.missing_delegation_record.iter().all(
                        |(missing_pubkey, _)| missing_pubkey != &pubkey,
                    ) =>
            {
                let bank_slot = self
                    .accounts_bank
                    .get_account(&pubkey)
                    .map(|in_bank| in_bank.remote_slot());
                if bank_slot.is_none_or(|slot| slot < account.remote_slot()) {
                    trace!(
                        pubkey = %pubkey,
                        bank_slot,
                        update_slot = account.remote_slot(),
                        ?result,
                        "Greedy clone did not materialize a fresh enough account; falling back"
                    );
                    false
                } else if let Some(projected_ata_clone_request) = self
                    .maybe_build_projected_ata_clone_request_from_subscription_update(
                        pubkey,
                        &account,
                        Some(&deleg_record),
                        &delegation_actions,
                    )
                    .await
                {
                    let projected_ata_pubkey =
                        projected_ata_clone_request.pubkey;
                    if let Err(err) = self
                        .clone_projected_ata_request(
                            projected_ata_clone_request,
                        )
                        .await
                    {
                        warn!(
                            pubkey = %pubkey,
                            error = %err,
                            "Failed to clone projected ATA from greedily discovered delegated eATA"
                        );
                        false
                    } else {
                        trace!(
                            pubkey = %pubkey,
                            ata_pubkey = %projected_ata_pubkey,
                            slot = account.remote_slot(),
                            "Greedily cloned delegated account"
                        );
                        true
                    }
                } else {
                    let cloned_ata_pubkey = greedy_ata_pubkeys
                        .iter()
                        .copied()
                        .find(|ata_pubkey| {
                            self.accounts_bank
                                .get_account(ata_pubkey)
                                .is_some_and(|account_in_bank| {
                                    account_in_bank.remote_slot()
                                        >= account.remote_slot()
                                })
                        });
                    if let Some(ata_pubkey) = cloned_ata_pubkey {
                        trace!(
                            pubkey = %pubkey,
                            ata_pubkey = %ata_pubkey,
                            slot = account.remote_slot(),
                            "Greedily cloned delegated account"
                        );
                    } else {
                        trace!(
                            pubkey = %pubkey,
                            slot = account.remote_slot(),
                            "Greedily cloned delegated account"
                        );
                    }
                    true
                }
            }
            Ok(result) => {
                trace!(
                    pubkey = %pubkey,
                    ?result,
                    "Greedy clone incomplete; falling back"
                );
                false
            }
            Err(err) => {
                warn!(
                    pubkey = %pubkey,
                    error = %err,
                    "Failed to greedily clone discovered delegated account"
                );
                false
            }
        }
    }

    async fn handle_executable_sub_update(
        &self,
        pubkey: Pubkey,
        account: AccountSharedData,
    ) {
        // moved to program_loader module
        program_loader::handle_executable_sub_update(self, pubkey, account)
            .await;
    }

    async fn cleanup_direct_subscription_for_delegated_account(
        &self,
        pubkey: Pubkey,
    ) {
        if let Err(err) = self
            .remote_account_provider
            .release_subscription_reason_silently_for_delegated_account(
                &pubkey,
                SubscriptionReason::DirectAccount,
            )
            .await
        {
            warn!(
                pubkey = %pubkey,
                error = %err,
                "Failed to clean up direct subscription for delegated account"
            );
        }
    }

    async fn ensure_direct_subscription_for_completed_account(
        &self,
        pubkey: Pubkey,
    ) {
        if let Err(err) = self
            .remote_account_provider
            .ensure_subscription(&pubkey, SubscriptionReason::DirectAccount)
            .await
        {
            warn!(
                pubkey = %pubkey,
                error = %err,
                "Failed to retain direct subscription for completed account"
            );
        }
    }

    async fn cleanup_undelegation_tracking_for_completed_account(
        &self,
        pubkey: Pubkey,
    ) {
        if let Err(err) = self
            .remote_account_provider
            .release_subscription_reason_silently_for_delegated_account(
                &pubkey,
                SubscriptionReason::UndelegationTracking,
            )
            .await
        {
            warn!(
                pubkey = %pubkey,
                error = %err,
                "Failed to clean up undelegation tracking for completed account"
            );
        }
    }

    async fn resolve_account_to_clone_from_forwarded_sub_with_unsubscribe(
        &self,
        update: ForwardedSubscriptionUpdate,
    ) -> (
        Option<AccountSharedData>,
        Option<DelegationRecord>,
        DelegationActions,
    ) {
        let ForwardedSubscriptionUpdate {
            pubkey,
            account,
            source: _,
        } = update;
        let owned_by_delegation_program =
            account.is_owned_by_delegation_program();

        if let Some(account) = account.fresh_account() {
            // If the account is owned by the delegation program we need to resolve
            // its true owner and determine if it is delegated to us
            if owned_by_delegation_program {
                let delegation_record_pubkey =
                    delegation_record_pda_from_delegated_account(&pubkey);

                let acquired_delegation_record_reason = self
                    .acquire_subscription_reason(
                        &delegation_record_pubkey,
                        SubscriptionReason::DelegationRecord,
                    )
                    .await
                    .map(|_| true)
                    .unwrap_or_else(|err| {
                        warn!(
                            pubkey = %delegation_record_pubkey,
                            error = ?err,
                            "Failed to acquire delegation record subscription reason"
                        );
                        false
                    });

                match self
                    .task_to_fetch_with_companion(
                        pubkey,
                        delegation_record_pubkey,
                        account.remote_slot(),
                        AccountFetchOrigin::GetAccount,
                    )
                    .await
                {
                    Ok(Ok(AccountWithCompanion {
                        pubkey,
                        mut account,
                        companion_pubkey: delegation_record_pubkey,
                        companion_account: delegation_record,
                    })) => {
                        // We may need to remove temporary subscriptions created
                        // while resolving this update.
                        let mut subs_to_remove = Vec::new();

                        subs_to_remove.push(SubscriptionRelease::Pubkey {
                            pubkey: delegation_record_pubkey,
                            reason: SubscriptionReason::DirectAccount,
                        });
                        if acquired_delegation_record_reason {
                            subs_to_remove.push(SubscriptionRelease::Pubkey {
                                pubkey: delegation_record_pubkey,
                                reason: SubscriptionReason::DelegationRecord,
                            });
                        }

                        let account = if let Some(delegation_record) =
                            delegation_record
                        {
                            let delegation_record_with_actions = match self
                                .parse_delegation_record(
                                    delegation_record.data(),
                                    delegation_record_pubkey,
                                ) {
                                Ok(x) => Some(x),
                                Err(err) => {
                                    error!(
                                        pubkey = %pubkey,
                                        error = %err,
                                        "Failed to parse delegation record"
                                    );
                                    None
                                }
                            };

                            // If the delegation record is valid we set the owner and delegation
                            // status on the account
                            if let Some((
                                delegation_record,
                                delegation_actions,
                            )) = delegation_record_with_actions
                            {
                                if tracing::enabled!(tracing::Level::TRACE) {
                                    let delegation_record_display =
                                        format!("{:?}", delegation_record);
                                    trace!(
                                        pubkey = %pubkey,
                                        slot = account.remote_slot(),
                                        owner = %delegation_record.owner,
                                        deleg_record = %delegation_record_display,
                                        "Resolving delegated account"
                                    );
                                }

                                self.apply_delegation_record_to_account(
                                    pubkey,
                                    &mut account,
                                    &delegation_record,
                                );

                                // For accounts delegated to us, subscribe to the original owner
                                // program for undelegation update resilience.
                                if account.delegated()
                                    && !self
                                        .programs_not_to_subscribe
                                        .contains(&delegation_record.owner)
                                {
                                    // Fire-and-forget to avoid blocking subscription updates.
                                    let provider =
                                        self.remote_account_provider.clone();
                                    let owner = delegation_record.owner;
                                    tokio::spawn(async move {
                                        if let Err(err) = provider
                                            .subscribe_program(owner)
                                            .await
                                        {
                                            warn!(
                                                "Failed to subscribe to owner program {} for account {}: {}",
                                                owner, pubkey, err
                                            );
                                        }
                                    });
                                }

                                (
                                    Some(account.into_account_shared_data()),
                                    Some(delegation_record),
                                    delegation_actions.unwrap_or_default(),
                                )
                            } else {
                                // If the delegation record is invalid we cannot clone the account
                                // since something is corrupt and we wouldn't know what owner to
                                // use, etc.
                                (None, None, DelegationActions::default())
                            }
                        } else if is_internal_dlp_account_data(account.data()) {
                            (
                                Some(account.into_account_shared_data()),
                                None,
                                DelegationActions::default(),
                            )
                        } else {
                            trace!(
                                pubkey = %pubkey,
                                "Skipping DLP-owned subscription update without delegation record"
                            );
                            (None, None, DelegationActions::default())
                        };

                        if !subs_to_remove.is_empty() {
                            release_subs(
                                &self.remote_account_provider,
                                subs_to_remove,
                            )
                            .await;
                        }
                        account
                    }
                    // In case of errors fetching the delegation record we cannot clone the account
                    Ok(Err(err)) => {
                        warn!(
                            pubkey = %pubkey,
                            error = ?err,
                            "Failed to fetch delegation record"
                        );
                        if acquired_delegation_record_reason {
                            release_subs(
                                &self.remote_account_provider,
                                [SubscriptionRelease::Pubkey {
                                    pubkey: delegation_record_pubkey,
                                    reason:
                                        SubscriptionReason::DelegationRecord,
                                }],
                            )
                            .await;
                        }
                        (None, None, DelegationActions::default())
                    }
                    Err(err) => {
                        warn!(
                            pubkey = %pubkey,
                            error = ?err,
                            "Failed to fetch delegation record"
                        );
                        if acquired_delegation_record_reason {
                            release_subs(
                                &self.remote_account_provider,
                                [SubscriptionRelease::Pubkey {
                                    pubkey: delegation_record_pubkey,
                                    reason:
                                        SubscriptionReason::DelegationRecord,
                                }],
                            )
                            .await;
                        }
                        (None, None, DelegationActions::default())
                    }
                }
            } else {
                let (account, deleg_record) = self
                    .maybe_project_ata_from_subscription_update(pubkey, account)
                    .await;
                if let Some((deleg_record, actions)) = deleg_record {
                    (
                        Some(account),
                        Some(deleg_record),
                        actions.unwrap_or_default(),
                    )
                } else {
                    (Some(account), None, DelegationActions::default())
                }
            }
        } else {
            // This should not happen since we call this method with sub updates which always hold
            // a fresh remote account
            error!(pubkey = %pubkey, account = ?account, "BUG: Received subscription update without fresh account");
            (None, None, DelegationActions::default())
        }
    }

    async fn maybe_build_projected_ata_clone_request_from_subscription_update(
        &self,
        eata_pubkey: Pubkey,
        eata_account: &AccountSharedData,
        deleg_record: Option<&DelegationRecord>,
        delegation_actions: &DelegationActions,
    ) -> Option<AccountCloneRequest> {
        ata_projection::maybe_build_projected_ata_clone_request_from_subscription_update(
            self,
            eata_pubkey,
            eata_account,
            deleg_record,
            delegation_actions,
        )
        .await
    }

    #[cfg(test)]
    fn is_known_empty_eata(&self, eata_pubkey: &Pubkey) -> bool {
        ata_projection::is_known_empty_eata(self, eata_pubkey)
    }

    async fn maybe_project_ata_from_subscription_update(
        &self,
        ata_pubkey: Pubkey,
        ata_account: AccountSharedData,
    ) -> (
        AccountSharedData,
        Option<(DelegationRecord, Option<DelegationActions>)>,
    ) {
        ata_projection::maybe_project_ata_from_subscription_update(
            self,
            ata_pubkey,
            ata_account,
        )
        .await
    }

    /// Parses a delegation record from account data bytes.
    /// Returns the parsed DelegationRecord, or InvalidDelegationRecord error
    /// if parsing fails.
    fn parse_delegation_record(
        &self,
        data: &[u8],
        delegation_record_pubkey: Pubkey,
    ) -> ChainlinkResult<(DelegationRecord, Option<DelegationActions>)> {
        delegation::parse_delegation_record(
            data,
            delegation_record_pubkey,
            self.validator_keypair.as_ref(),
        )
    }

    /// Applies delegation record settings to an account: sets the owner,
    /// delegation status, and confined status based on the delegation
    /// record's authority field.
    /// Returns commit frequency if account is delegated to us
    fn apply_delegation_record_to_account(
        &self,
        account_pubkey: Pubkey,
        account: &mut ResolvedAccountSharedData,
        delegation_record: &DelegationRecord,
    ) -> Option<u64> {
        delegation::apply_delegation_record_to_account(
            self,
            account_pubkey,
            account,
            delegation_record,
        )
    }

    /// Returns the pubkey of another validator if account is delegated to them,
    /// None if delegated to us or delegated to the system program (confined).
    fn get_delegated_to_other(
        &self,
        delegation_record: &DelegationRecord,
    ) -> Option<Pubkey> {
        delegation::get_delegated_to_other(self, delegation_record)
    }

    /// Fetches and parses the delegation record for an account, returning the
    /// parsed DelegationRecord if found and valid, None otherwise.
    async fn fetch_and_parse_delegation_record(
        &self,
        account_pubkey: Pubkey,
        min_context_slot: u64,
        fetch_origin: metrics::AccountFetchOrigin,
    ) -> Option<(DelegationRecord, Option<DelegationActions>)> {
        delegation::fetch_and_parse_delegation_record(
            self,
            account_pubkey,
            min_context_slot,
            fetch_origin,
        )
        .await
    }

    /// Tries to fetch all accounts in `pubkeys` and clone them into the bank.
    /// If `mark_empty` is provided, accounts in that list that are
    /// not found on chain will be added with zero lamports to the bank.
    ///
    /// - **pubkeys**: list of accounts to fetch and clone
    /// - **mark_empty**: optional list of accounts that should be added as empty if not found on
    ///   chain
    /// - **slot**: optional slot to use as minimum context slot for the accounts being cloned
    ///
    /// NOTE: accounts fetched here have not been found in the bank
    #[instrument(skip(self, pubkeys, mark_empty_if_not_found), fields(tx_sig = tracing::field::Empty))]
    async fn fetch_and_clone_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_origin: AccountFetchOrigin,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        if let Some(sig) = fetch_origin.signature() {
            tracing::Span::current().record("tx_sig", sig.to_string());
        }
        if tracing::enabled!(tracing::Level::TRACE) {
            let pubkeys_count = pubkeys.len();
            trace!(count = pubkeys_count, "Fetching and cloning accounts");
        }

        // Increment fetch counter for testing deduplication (count per account being fetched)
        self.fetch_count
            .fetch_add(pubkeys.len() as u64, Ordering::Relaxed);

        // Keep the main account fetch aligned with the freshest observed slot.
        let min_context_slot = slot.map(|subscription_slot| {
            subscription_slot.max(self.remote_account_provider.chain_slot())
        });

        let accs = self
            .remote_account_provider
            .try_get_multi(
                pubkeys,
                mark_empty_if_not_found,
                fetch_origin,
                min_context_slot,
            )
            .await?;

        if tracing::enabled!(tracing::Level::TRACE) {
            let accs_count = accs.len();
            trace!(count = accs_count, "Fetched accounts");
        }

        let ClassifiedAccounts {
            not_found,
            plain,
            owned_by_deleg,
            programs,
            atas,
        } = pipeline::classify_remote_accounts(accs, pubkeys);

        if tracing::enabled!(tracing::Level::TRACE) {
            let not_found = not_found
                .iter()
                .map(|(pubkey, slot)| (pubkey.to_string(), *slot))
                .collect::<Vec<_>>();
            let plain = plain
                .iter()
                .map(|p| p.pubkey.to_string())
                .collect::<Vec<_>>();
            let owned_by_deleg = owned_by_deleg
                .iter()
                .map(|(pubkey, _, slot)| (pubkey.to_string(), *slot))
                .collect::<Vec<_>>();
            let programs = programs
                .iter()
                .map(|(p, _, _)| p.to_string())
                .collect::<Vec<_>>();
            let atas = atas
                .iter()
                .map(|(a, _, _, _)| a.to_string())
                .collect::<Vec<_>>();
            trace!(
                "Fetched accounts: \nnot_found:      {not_found:?} \nplain:          {plain:?} \nowned_by_deleg: {owned_by_deleg:?}\nprograms:       {programs:?} \natas:       {atas:?}",
            );
        }

        let PartitionedNotFound {
            clone_as_empty,
            not_found,
        } = pipeline::partition_not_found(mark_empty_if_not_found, not_found);

        // For accounts we couldn't find we cannot do anything. We will let code depending
        // on them to be in the bank fail on its own
        if !not_found.is_empty() {
            trace!(
                "Could not find accounts on chain: {:?}",
                not_found
                    .iter()
                    .map(|(pubkey, slot)| (pubkey.to_string(), *slot))
                    .collect::<Vec<_>>()
            );
        }

        // We mark some accounts as empty if we know that they will never exist on chain
        if tracing::enabled!(tracing::Level::TRACE)
            && !clone_as_empty.is_empty()
        {
            trace!(
                "Cloning accounts as empty: {:?}",
                clone_as_empty
                    .iter()
                    .map(|(p, _)| p.to_string())
                    .collect::<Vec<_>>()
            );
        }

        // For potentially delegated accounts we update the owner and delegation state first
        let ResolvedDelegatedAccounts {
            mut accounts_to_clone,
            mut record_subs,
            missing_delegation_record,
        } = match pipeline::resolve_delegated_accounts(
            self,
            owned_by_deleg,
            plain,
            min_context_slot,
            fetch_origin,
        )
        .await
        {
            Ok(resolved) => resolved,
            Err(err) => {
                release_subs(
                    &self.remote_account_provider,
                    pubkeys.iter().copied().map(|pubkey| {
                        SubscriptionRelease::Pubkey {
                            pubkey,
                            reason: SubscriptionReason::DirectAccount,
                        }
                    }),
                )
                .await;
                return Err(err);
            }
        };

        let ResolvedPrograms {
            loaded_programs,
            mut program_data_subs,
        } = match pipeline::resolve_programs_with_program_data(
            self,
            programs,
            min_context_slot,
            fetch_origin,
        )
        .await
        {
            Ok(resolved) => resolved,
            Err(err) => {
                let releases = pubkeys
                    .iter()
                    .copied()
                    .map(|pubkey| SubscriptionRelease::Pubkey {
                        pubkey,
                        reason: SubscriptionReason::DirectAccount,
                    })
                    .chain(record_subs.iter().copied().map(|pubkey| {
                        SubscriptionRelease::Pubkey {
                            pubkey,
                            reason: SubscriptionReason::DirectAccount,
                        }
                    }))
                    .chain(record_subs.iter().copied().map(|pubkey| {
                        SubscriptionRelease::Pubkey {
                            pubkey,
                            reason: SubscriptionReason::DelegationRecord,
                        }
                    }))
                    .collect::<Vec<_>>();
                release_subs(&self.remote_account_provider, releases).await;
                return Err(err);
            }
        };

        let mut loaded_programs = loaded_programs;
        let mut all_requested_pubkeys = pubkeys.to_vec();
        all_requested_pubkeys.extend(record_subs.iter().copied());
        all_requested_pubkeys.extend(program_data_subs.iter().copied());

        // We will compute subscription cancellations after ATA handling, once accounts_to_clone is finalized

        // Handle ATAs: for each detected ATA, we derive the eATA PDA, subscribe to both,
        // and, if the ATA is delegated to us and the eATA exists, we clone the eATA data
        // into the ATA in the bank.
        // eATA subscriptions are kept implicitly (not tracked for release).
        let ata_accounts = ata_projection::resolve_ata_with_eata_projection(
            self,
            atas,
            min_context_slot,
            fetch_origin,
        )
        .await;
        accounts_to_clone.extend(ata_accounts);

        // Ensure all accounts referenced by delegation actions exist and are
        // cloned before we execute those actions as part of account cloning.
        let action_dependencies =
            pipeline::collect_delegation_action_dependencies(
                &accounts_to_clone,
            );
        let action_dependencies_to_fetch = action_dependencies
            .into_iter()
            .filter(|dependency| {
                self.accounts_bank.get_account(dependency).is_none()
                    && !accounts_to_clone
                        .iter()
                        .any(|request| request.pubkey.eq(dependency))
                    && !loaded_programs
                        .iter()
                        .any(|program| program.program_id.eq(dependency))
            })
            .collect::<Vec<_>>();

        if !action_dependencies_to_fetch.is_empty() {
            if tracing::enabled!(tracing::Level::TRACE) {
                trace!(
                    dependencies = ?action_dependencies_to_fetch,
                    "Ensuring delegation action dependencies"
                );
            }

            self.fetch_count.fetch_add(
                action_dependencies_to_fetch.len() as u64,
                Ordering::Relaxed,
            );
            let action_dep_accs = self
                .remote_account_provider
                .try_get_multi(
                    &action_dependencies_to_fetch,
                    None,
                    fetch_origin,
                    min_context_slot,
                )
                .await?;
            all_requested_pubkeys
                .extend(action_dependencies_to_fetch.iter().copied());

            let ClassifiedAccounts {
                not_found,
                plain,
                owned_by_deleg,
                programs,
                atas,
            } = pipeline::classify_remote_accounts(
                action_dep_accs,
                &action_dependencies_to_fetch,
            );

            if tracing::enabled!(tracing::Level::TRACE) && !not_found.is_empty()
            {
                trace!(
                    dependencies = ?not_found,
                    "Delegation action dependencies not found on chain; continuing clone flow"
                );
            }

            let ResolvedDelegatedAccounts {
                accounts_to_clone: action_dep_accounts_to_clone,
                record_subs: action_dep_record_subs,
                missing_delegation_record: action_dep_missing_delegation_record,
            } = match pipeline::resolve_delegated_accounts(
                self,
                owned_by_deleg,
                plain,
                min_context_slot,
                fetch_origin,
            )
            .await
            {
                Ok(resolved) => resolved,
                Err(err) => {
                    let releases = pipeline::compute_subscription_releases(
                        &all_requested_pubkeys,
                        &accounts_to_clone,
                        &loaded_programs,
                        record_subs.clone(),
                        program_data_subs.clone(),
                    );
                    release_subs(&self.remote_account_provider, releases).await;
                    return Err(err);
                }
            };

            if !action_dep_missing_delegation_record.is_empty() {
                let releases = pipeline::compute_subscription_releases(
                    &all_requested_pubkeys,
                    &accounts_to_clone,
                    &loaded_programs,
                    record_subs
                        .iter()
                        .copied()
                        .chain(action_dep_record_subs.iter().copied())
                        .collect(),
                    program_data_subs.clone(),
                );
                release_subs(&self.remote_account_provider, releases).await;
                return Err(ChainlinkError::MissingDelegationActionAccounts(
                    action_dep_missing_delegation_record
                        .iter()
                        .map(|(pubkey, _)| *pubkey)
                        .collect(),
                ));
            }

            all_requested_pubkeys
                .extend(action_dep_record_subs.iter().copied());
            record_subs.extend(action_dep_record_subs);

            let ResolvedPrograms {
                loaded_programs: action_dep_loaded_programs,
                program_data_subs: action_dep_program_data_subs,
            } = match pipeline::resolve_programs_with_program_data(
                self,
                programs,
                min_context_slot,
                fetch_origin,
            )
            .await
            {
                Ok(resolved) => resolved,
                Err(err) => {
                    let mut cleanup_accounts_to_clone =
                        accounts_to_clone.clone();
                    cleanup_accounts_to_clone
                        .extend(action_dep_accounts_to_clone.clone());
                    let releases = pipeline::compute_subscription_releases(
                        &all_requested_pubkeys,
                        &cleanup_accounts_to_clone,
                        &loaded_programs,
                        record_subs.clone(),
                        program_data_subs.clone(),
                    );
                    release_subs(&self.remote_account_provider, releases).await;
                    return Err(err);
                }
            };

            all_requested_pubkeys
                .extend(action_dep_program_data_subs.iter().copied());
            program_data_subs.extend(action_dep_program_data_subs);

            let action_dep_ata_accounts =
                ata_projection::resolve_ata_with_eata_projection(
                    self,
                    atas,
                    min_context_slot,
                    fetch_origin,
                )
                .await;

            accounts_to_clone.extend(action_dep_accounts_to_clone);
            accounts_to_clone.extend(action_dep_ata_accounts);
            loaded_programs.extend(action_dep_loaded_programs);
        }

        let releases = pipeline::compute_subscription_releases(
            &all_requested_pubkeys,
            &accounts_to_clone,
            &loaded_programs,
            record_subs,
            program_data_subs,
        );

        pipeline::clone_accounts_and_programs(
            self,
            accounts_to_clone,
            loaded_programs,
        )
        .await?;

        release_subs(&self.remote_account_provider, releases).await;

        Ok(FetchAndCloneResult {
            not_found_on_chain: not_found,
            missing_delegation_record,
        })
    }

    /// Determines if the account finished undelegating on chain.
    /// If it has finished undelegating, we should refresh it in the bank.
    /// - **pubkey**: the account pubkey
    /// - **in_bank**: the account as it exists in the bank
    ///
    /// Returns true if the account should be refreshed in the bank
    async fn should_refresh_undelegating_in_bank_account(
        &self,
        pubkey: &Pubkey,
        in_bank: &AccountSharedData,
        fetch_origin: AccountFetchOrigin,
    ) -> RefreshDecision {
        if in_bank.undelegating() {
            debug!(
                pubkey = %pubkey,
                delegated = in_bank.delegated(),
                undelegating = in_bank.undelegating(),
                "Fetching undelegating account"
            );

            if let Some(eata_pubkey) =
                ata_projection::derive_eata_pubkey_from_ata_layout(
                    pubkey, in_bank,
                )
            {
                let projected_deleg_record = self
                    .fetch_and_parse_delegation_record(
                        eata_pubkey,
                        self.remote_account_provider.chain_slot(),
                        fetch_origin,
                    )
                    .await;
                if projected_deleg_record.as_ref().is_some_and(|(record, _)| {
                    record.owner == EATA_PROGRAM_ID
                        && record.authority == self.validator_pubkey
                }) {
                    debug!(
                        pubkey = %pubkey,
                        eata_pubkey = %eata_pubkey,
                        "Keeping undelegating ATA in bank while companion eATA remains delegated"
                    );
                    return RefreshDecision::No;
                }
            }

            let deleg_record = self
                .fetch_and_parse_delegation_record(
                    *pubkey,
                    self.remote_account_provider.chain_slot(),
                    fetch_origin,
                )
                .await;

            if deleg_record.is_none() {
                // If there is no delegation record then it is possible that the account itself
                // does not exist either.
                // In that case we need to refresh it as empty to clear the undelegation state.
                return RefreshDecision::YesAndMarkEmptyIfNotFound;
            }

            let delegated_on_chain =
                deleg_record.as_ref().is_some_and(|(dr, _)| {
                    dr.authority.eq(&self.validator_pubkey)
                        || dr.authority.eq(&Pubkey::default())
                });
            let deleg_record = deleg_record.map(|el| el.0);
            if !account_still_undelegating_on_chain(
                pubkey,
                delegated_on_chain,
                in_bank.remote_slot(),
                deleg_record,
                &self.validator_pubkey,
            ) {
                debug!(
                    "Account {pubkey} marked as undelegating will be overridden since undelegation completed"
                );
                return RefreshDecision::Yes;
            }
        } else if in_bank.owner().eq(&dlp_api::id()) {
            debug!(
                "Account {pubkey} owned by deleg program not marked as undelegating"
            );
        }
        RefreshDecision::No
    }

    /// Fetch and clone accounts with request deduplication to avoid parallel fetches of the same account.
    /// This method implements the new logic where:
    /// 1. Check synchronously if account is in bank, return immediately if found
    /// 2. If account is pending, add to pending requests and await
    /// 3. Create pending entries and fetch via RemoteAccountProvider
    /// 4. Once fetched, clone into bank and respond to all pending requests
    /// 5. Clear pending requests for that account
    ///
    /// Note: since we fetch each account only once in parallel, we also avoid fetching
    /// the same delegation record in parallel.
    #[instrument(skip(self, pubkeys, mark_empty_if_not_found))]
    pub async fn fetch_and_clone_accounts_with_dedup(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_origin: AccountFetchOrigin,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        self.fetch_and_clone_accounts_with_dedup_forced_refresh(
            pubkeys,
            mark_empty_if_not_found,
            slot,
            fetch_origin,
            &HashSet::new(),
        )
        .await
    }

    async fn fetch_and_clone_accounts_with_dedup_forced_refresh(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_origin: AccountFetchOrigin,
        force_refresh_pubkeys: &HashSet<Pubkey>,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        // We cannot clone blacklisted accounts, thus either they are already
        // in the bank (e.g. native programs) or they don't exist and the transaction
        // will fail later
        let mut pubkeys = pubkeys
            .iter()
            .filter(|p| !self.blacklisted_accounts.contains(p))
            .collect::<Vec<_>>();
        if tracing::enabled!(tracing::Level::TRACE) {
            let count = pubkeys.len();
            trace!(count, "Fetching and cloning accounts with dedup");
        }

        let mut in_bank = HashSet::new();
        let mut extra_mark_empty = vec![];

        // Phase 1: Sync bank check — separate undelegating accounts
        // (which need async RPC) from non-undelegating (handled
        // synchronously)
        let mut undelegating_checks: Vec<(Pubkey, AccountSharedData)> = vec![];
        for pubkey in pubkeys.iter() {
            if force_refresh_pubkeys.contains(*pubkey) {
                continue;
            }
            if let Some(account_in_bank) =
                self.accounts_bank.get_account(pubkey)
            {
                if account_in_bank.undelegating() {
                    undelegating_checks.push((**pubkey, account_in_bank));
                } else {
                    if account_in_bank.owner().eq(&dlp_api::id()) {
                        debug!(
                            pubkey = %pubkey,
                            "Account owned by deleg program not marked as undelegating"
                        );
                    }
                    if tracing::enabled!(tracing::Level::TRACE) {
                        let delegated = account_in_bank.delegated();
                        let owner = account_in_bank.owner();
                        trace!(
                            pubkey = %pubkey,
                            undelegating = false,
                            delegated,
                            owner = %owner,
                            "Account found in bank in valid state, no fetch needed"
                        );
                    }
                    in_bank.insert(**pubkey);
                }
            }
        }

        // Phase 2: Parallel undelegation checks via JoinSet
        if !undelegating_checks.is_empty() {
            let mut join_set = JoinSet::new();
            for (pubkey, account_in_bank) in undelegating_checks {
                let this = self.clone();
                join_set.spawn(async move {
                    let decision = match tokio::time::timeout(
                        Duration::from_secs(5),
                        this.should_refresh_undelegating_in_bank_account(
                            &pubkey,
                            &account_in_bank,
                            fetch_origin,
                        ),
                    )
                    .await
                    {
                        Ok(decision) => decision,
                        Err(_timeout) => {
                            warn!(
                                pubkey = %pubkey,
                                "Timeout checking if account is still undelegating after 5 seconds"
                            );
                            RefreshDecision::No
                        }
                    };
                    (pubkey, decision)
                });
            }

            for (pubkey, decision) in join_set.join_all().await {
                match decision {
                    RefreshDecision::Yes
                    | RefreshDecision::YesAndMarkEmptyIfNotFound => {
                        debug!(
                            pubkey = %pubkey,
                            "Account completed undelegation which was missed and is fetched again"
                        );
                        metrics::inc_unstuck_undelegation_count();
                        if let RefreshDecision::YesAndMarkEmptyIfNotFound =
                            decision
                        {
                            extra_mark_empty.push(pubkey);
                        }
                    }
                    RefreshDecision::No => {
                        if tracing::enabled!(tracing::Level::TRACE) {
                            trace!(
                                pubkey = %pubkey,
                                "Undelegating account still valid, no fetch needed"
                            );
                        }
                        in_bank.insert(pubkey);
                    }
                }
            }
        }
        pubkeys.retain(|p| !in_bank.contains(p));

        let mut mark_empty_set = mark_empty_if_not_found
            .unwrap_or(&[])
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        mark_empty_set.extend(extra_mark_empty);

        let mut waiters: Vec<PendingWaiter> = vec![];
        for pubkey in pubkeys {
            match self.claim_or_join_owned_operation(*pubkey) {
                PendingClaim::Created(handles) => {
                    let PendingHandles {
                        waiter,
                        deadline,
                        cancel,
                        owner,
                    } = handles;
                    let waiter_pubkey = waiter.pubkey();
                    let Some(owner) = owner else {
                        cancel.notify_waiters();
                        finish_pending(
                            &self.pending_requests,
                            waiter_pubkey,
                            waiter.generation(),
                            PendingTerminal::Failed(PendingFailure::Cancelled),
                        );
                        return Err(
                            ChainlinkError::MissingPendingRequestOwner(
                                waiter_pubkey,
                            ),
                        );
                    };
                    self.spawn_owned_operation(
                        waiter_pubkey,
                        waiter.generation(),
                        deadline,
                        cancel,
                        owner,
                        mark_empty_set.contains(&waiter_pubkey),
                        slot,
                        fetch_origin,
                    );
                    waiters.push(waiter);
                }
                PendingClaim::Joined(handles) => waiters.push(handles.waiter),
            }
        }

        let mut final_result = FetchAndCloneResult {
            not_found_on_chain: vec![],
            missing_delegation_record: vec![],
        };
        for waiter in waiters {
            let pubkey = waiter.pubkey();
            match waiter.wait().await? {
                PendingTerminal::Success(owner_result) => {
                    for entry in owner_result.not_found_on_chain {
                        if entry.0 == pubkey {
                            final_result.not_found_on_chain.push(entry);
                        }
                    }
                    for entry in owner_result.missing_delegation_record {
                        if entry.0 == pubkey {
                            final_result.missing_delegation_record.push(entry);
                        }
                    }
                }
                PendingTerminal::Failed(failure) => {
                    return Err(failure.into_chainlink_error(pubkey));
                }
            }
        }

        Ok(final_result)
    }

    fn task_to_fetch_with_delegation_record(
        &self,
        pubkey: Pubkey,
        slot: u64,
        fetch_origin: AccountFetchOrigin,
    ) -> task::JoinHandle<ChainlinkResult<AccountWithCompanion>> {
        let delegation_record_pubkey =
            delegation_record_pda_from_delegated_account(&pubkey);
        self.task_to_fetch_with_companion(
            pubkey,
            delegation_record_pubkey,
            slot,
            fetch_origin,
        )
    }

    fn task_to_fetch_with_program_data(
        &self,
        pubkey: Pubkey,
        slot: u64,
        fetch_origin: AccountFetchOrigin,
    ) -> task::JoinHandle<ChainlinkResult<AccountWithCompanion>> {
        let program_data_pubkey =
            get_loaderv3_get_program_data_address(&pubkey);
        self.task_to_fetch_with_companion(
            pubkey,
            program_data_pubkey,
            slot,
            fetch_origin,
        )
    }

    fn task_to_fetch_with_companion(
        &self,
        pubkey: Pubkey,
        companion_pubkey: Pubkey,
        slot: u64,
        fetch_origin: AccountFetchOrigin,
    ) -> task::JoinHandle<ChainlinkResult<AccountWithCompanion>> {
        let provider = self.remote_account_provider.clone();
        let bank = self.accounts_bank.clone();
        let fetch_count = self.fetch_count.clone();
        task::spawn(async move {
            trace!(
                pubkey = %pubkey,
                companion = %companion_pubkey,
                slot,
                "Fetching account with companion"
            );

            // Increment fetch counter for testing deduplication (2 accounts: pubkey + delegation_record_pubkey)
            fetch_count.fetch_add(2, Ordering::Relaxed);

            provider
                .try_get_multi_until_slots_match(
                    &[pubkey, companion_pubkey],
                    Some(MatchSlotsConfig {
                        min_context_slot: Some(slot),
                        ..Default::default()
                    }),
                    fetch_origin,
                )
                .await
                .map_err(ChainlinkError::from)
                .and_then(|accs| {
                    match accs.as_slice() {
                        [acc_first, acc_last] => {
                            Ok((acc_first.clone(), acc_last.clone()))
                        }
                        _ => Err(ChainlinkError::UnexpectedAccountCount(format!(
                            "Expected exactly 2 accounts for pubkey {} and companion {}, got {}",
                            pubkey,
                            companion_pubkey,
                            accs.len()
                        ))),
                    }
                })
                .and_then(|(acc, deleg)| {
                    Self::resolve_account_with_companion(
                        &bank,
                        pubkey,
                        companion_pubkey,
                        acc,
                        deleg,
                    )
                })
        })
    }

    fn resolve_account_with_companion(
        bank: &V,
        pubkey: Pubkey,
        companion_pubkey: Pubkey,
        acc: RemoteAccount,
        companion: RemoteAccount,
    ) -> ChainlinkResult<AccountWithCompanion> {
        use RemoteAccount::*;
        match (acc, companion) {
            // Account not found even though we found it previously - this is invalid,
            // either way we cannot use it now
            (NotFound(_), NotFound(_)) | (NotFound(_), Found(_)) => {
                Err(ChainlinkError::ResolvedAccountCouldNoLongerBeFound(pubkey))
            }
            (Found(acc), NotFound(_)) => {
                // Only account found without a companion
                // In case of delegation record fetch the account is either invalid
                // or a delegation record itself.
                // Clone it as is (without changing the owner or flagging as delegated)
                match acc.account.resolved_account_shared_data(bank) {
                    Some(account) => Ok(AccountWithCompanion {
                        pubkey,
                        account,
                        companion_pubkey,
                        companion_account: None,
                    }),
                    None => Err(
                        ChainlinkError::ResolvedAccountCouldNoLongerBeFound(
                            pubkey,
                        ),
                    ),
                }
            }
            (Found(acc), Found(comp)) => {
                // Found the delegation record, we include it so that the caller can
                // use it to add metadata to the account and use it for decision making
                let Some(comp_account) =
                    comp.account.resolved_account_shared_data(bank)
                else {
                    return Err(
                        ChainlinkError::ResolvedCompanionAccountCouldNoLongerBeFound(
                            companion_pubkey,
                        ),
                    );
                };
                let Some(account) =
                    acc.account.resolved_account_shared_data(bank)
                else {
                    return Err(
                        ChainlinkError::ResolvedAccountCouldNoLongerBeFound(
                            pubkey,
                        ),
                    );
                };
                Ok(AccountWithCompanion {
                    pubkey,
                    account,
                    companion_pubkey,
                    companion_account: Some(comp_account),
                })
            }
        }
    }

    /// Check if an account is currently being watched (subscribed to) by the
    /// remote account provider
    pub fn is_watching(&self, pubkey: &Pubkey) -> bool {
        self.remote_account_provider.is_watching(pubkey)
    }

    /// Subscribe to updates for a specific account
    /// This is typically used when an account is about to be undelegated
    /// and we need to start watching for changes
    #[instrument(skip(self))]
    pub(crate) async fn acquire_subscription_reason(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> ChainlinkResult<()> {
        self.remote_account_provider
            .acquire_subscription(pubkey, reason)
            .await
            .map_err(|err| {
                ChainlinkError::FailedToSubscribeToAccount(*pubkey, err)
            })
    }

    pub(crate) async fn ensure_subscription(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> ChainlinkResult<()> {
        self.remote_account_provider
            .ensure_subscription(pubkey, reason)
            .await
            .map_err(|err| {
                ChainlinkError::FailedToSubscribeToAccount(*pubkey, err)
            })
    }

    #[instrument(skip(self))]
    pub async fn subscribe_to_account_to_track_undelegation(
        &self,
        pubkey: &Pubkey,
    ) -> ChainlinkResult<()> {
        trace!(
            pubkey = %pubkey,
            reason = ?SubscriptionReason::UndelegationTracking,
            "Subscribing to account"
        );
        // Acquire undelegation tracking ownership before/with local
        // undelegating visibility; any LRU entry created for this reason is
        // protected from capacity eviction by the provider's bank-state
        // predicate and ownership filter.
        self.acquire_subscription_reason(
            pubkey,
            SubscriptionReason::UndelegationTracking,
        )
        .await
    }

    pub fn chain_slot(&self) -> u64 {
        self.remote_account_provider.chain_slot()
    }

    pub fn received_updates_count(&self) -> u64 {
        self.remote_account_provider.received_updates_count()
    }

    pub(crate) fn promote_accounts(&self, pubkeys: &[&Pubkey]) {
        self.remote_account_provider.promote_accounts(pubkeys);
    }

    pub fn try_get_removed_account_rx(
        &self,
    ) -> ChainlinkResult<mpsc::Receiver<Pubkey>> {
        Ok(self.remote_account_provider.try_get_removed_account_rx()?)
    }

    /// Best-effort airdrop helper: if the account doesn't exist in the bank or has 0 lamports,
    /// create/overwrite it as a plain system account with the provided lamports using the cloner path.
    #[instrument(skip(self))]
    pub async fn airdrop_account_if_empty(
        &self,
        pubkey: Pubkey,
        lamports: u64,
    ) -> ClonerResult<()> {
        if lamports == 0 {
            return Ok(());
        }
        let remote_slot =
            if let Some(acc) = self.accounts_bank.get_account(&pubkey) {
                if acc.lamports() > 0 {
                    return Ok(());
                }
                acc.remote_slot()
                    .max(self.remote_account_provider.chain_slot())
            } else {
                self.remote_account_provider.chain_slot()
            };
        // Build a plain system account with the requested balance
        let mut account =
            AccountSharedData::new(lamports, 0, &system_program::id());
        account.set_remote_slot(remote_slot);
        debug!(
            pubkey = %pubkey,
            lamports,
            remote_slot,
            "Auto-airdropping account"
        );
        let _sig = self
            .cloner
            .clone_account(AccountCloneRequest {
                pubkey,
                account,
                commit_frequency_ms: None,
                delegation_actions: DelegationActions::default(),
                delegated_to_other: None,
            })
            .await?;
        Ok(())
    }
}
