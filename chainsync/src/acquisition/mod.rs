//! Synchronizes selected targets, resolving companions before action dependencies.

mod plan;
mod resolve;

use std::{
    collections::{BTreeMap, BTreeSet},
    mem::take,
};

use futures::future;
use solana_account::AccountBuilder;
use solana_pubkey::Pubkey;

use self::{
    plan::FetchPlan,
    resolve::{BatchOutcome, PendingDelegation, prepare_programs},
};
use crate::{
    AccountSubscription, ChainSync, ChainSyncAccount, Result, SyncTarget,
    http::BatchKind,
    metrics::{self, Op},
    websocket::{Reference, Retention},
};

/// State shared by primary fetches, companion fetches, and action dependencies.
pub(super) struct Acquisition<'engine> {
    /// Supplies transports, normalization policy, and materialization into Engine.
    sync: &'engine ChainSync,
    /// Newly acquired references still owned by failure cleanup, keyed by remote address.
    /// Reused resident references are excluded; completed mirrors keep their own coverage.
    pending: BTreeSet<Pubkey>,
    /// Minimum response context slot, carried monotonically across dependency waves.
    floor: Option<u64>,
    /// HTTP entries fetched, including companions, nulls, and repeated primary fetches.
    count: u64,
    /// Primary passes entered across dependency waves, including passes that fail.
    rounds: usize,
    /// Readonly raw companions installed only after all target leases have been released.
    raw: BTreeMap<Pubkey, AccountBuilder>,
}

impl<'engine> Acquisition<'engine> {
    /// Starts with no account-selection policy; every input target is already leased.
    pub(super) fn new(sync: &'engine ChainSync, floor: Option<u64>) -> Self {
        Self {
            sync,
            pending: BTreeSet::new(),
            floor,
            count: 0,
            rounds: 0,
            raw: BTreeMap::new(),
        }
    }

    /// Releases unfinished references on failure without retiring completed mirrors.
    pub(super) async fn run(mut self, targets: Vec<SyncTarget<'engine>>) -> (Result<u64>, usize) {
        let _timer = metrics::time(Op::ChainSync);
        if targets.is_empty() {
            return (Ok(0), 0);
        }
        let result = self.waves(targets).await;
        self.sync.unsubscribe(self.pending).await;
        (result.map(|()| self.count), self.rounds)
    }

    /// Resolves a whole wave before selecting action dependencies under new leases.
    async fn waves(&mut self, mut targets: Vec<SyncTarget<'engine>>) -> Result<()> {
        let mut deferred = Vec::new();
        while !targets.is_empty() {
            let pending = self.wave(targets).await?;
            let mut next = BTreeMap::new();
            let mut ready = Vec::with_capacity(pending.len());
            for PendingDelegation { delegation, unsubscribe } in pending {
                let prepared = self.sync.prepare_delegation(delegation, &mut next)?;
                ready.push((prepared, unsubscribe));
            }
            deferred.push(ready);
            let requests = next
                .into_iter()
                .map(|(pubkey, property)| ChainSyncAccount { pubkey, property })
                .collect();
            targets = self.sync.missing(requests).await?;
            for target in &mut targets {
                // A dependency may revisit a delegation already deferred by this acquisition.
                target.subscribed = self.pending.contains(&target.accessor.pubkey());
            }
        }
        // Children must activate before the actions that requested their dependencies.
        for (prepared, unsubscribe) in deferred.into_iter().rev().flatten() {
            let pubkey = prepared.pubkey;
            self.sync.materialize_delegation(prepared).await?;
            if unsubscribe {
                self.sync.unsubscribe([pubkey]).await;
                self.pending.remove(&pubkey);
            }
        }
        Ok(())
    }

    /// Companion retries retain their selected leases; no account is reselected by pubkey.
    async fn wave(
        &mut self,
        mut targets: Vec<SyncTarget<'engine>>,
    ) -> Result<Vec<PendingDelegation>> {
        let mut pending = Vec::new();
        while !targets.is_empty() {
            self.rounds += 1;
            let mut inputs = targets.into_iter().peekable();
            let mut next = Vec::new();
            let mut min_context_slot = None;
            while inputs.peek().is_some() {
                let plan = FetchPlan::next(&mut inputs);
                let outcome = self.batch(plan).await?;
                next.extend(outcome.refetches);
                pending.extend(outcome.delegations);
                min_context_slot = min_context_slot.max(outcome.min_context_slot);
            }
            self.floor = self.floor.max(min_context_slot);
            targets = next;
        }
        // A raw companion may also be a leased primary in this wave. Acquire it
        // only now, before dependencies or actions can observe the projection.
        for (pubkey, account) in take(&mut self.raw) {
            self.sync.engine.account(pubkey).await?.materialize(account, None).await?;
            self.sync.unsubscribe([pubkey]).await;
            self.pending.remove(&pubkey);
        }
        Ok(pending)
    }

    /// Subscribes before fetching and installs only fully resolved account images.
    async fn batch(&mut self, plan: FetchPlan<'engine>) -> Result<BatchOutcome<'engine>> {
        let sync = self.sync;
        self.pending.extend(sync.subscribe(&plan.subscriptions).await?);
        let mut fetched = sync.fetcher.fetch(BatchKind::Primary, &plan.keys, self.floor).await?;
        self.count += plan.keys.len() as u64;
        let prune = prepare_programs(&plan, &mut fetched, &sync.engine.rent())?;
        let projected = self.fetch_ata_companions(&plan, &fetched).await?;
        // Retire unused program subscriptions before transferring completed mirrors
        // out of cleanup ownership, including when a later target fails materialization.
        sync.unsubscribe(prune.iter().copied()).await;
        for pubkey in prune {
            self.pending.remove(&pubkey);
        }
        self.materialize_batch(plan.accounts, &mut fetched, projected).await
    }

    /// Relinquishes cleanup ownership once a target has installed or retired its subscriptions.
    fn complete(&mut self, pubkey: Pubkey) {
        for sub in AccountSubscription::for_account(pubkey) {
            self.pending.remove(&sub.pubkey);
        }
    }
}

impl ChainSync {
    /// Waits for every request; partial failure releases only references acquired by this batch.
    async fn subscribe(
        &self,
        subscriptions: &[(AccountSubscription, Retention)],
    ) -> Result<Vec<Pubkey>> {
        let requests = subscriptions
            .iter()
            .map(|&(sub, retention)| self.websocket.subscribe(sub, retention));
        let mut subscribed = Vec::with_capacity(subscriptions.len());
        let mut failure = None;
        // Waiting for all results prevents a successful parallel request from going untracked.
        for ((subscription, _), result) in
            subscriptions.iter().zip(future::join_all(requests).await)
        {
            match result {
                Ok(Reference::Acquired) => subscribed.push(subscription.pubkey),
                Ok(Reference::Reused) => {}
                Err(error) => {
                    failure.replace(error);
                }
            }
        }
        if let Some(error) = failure {
            self.unsubscribe(subscribed).await;
            return Err(error.into());
        }
        Ok(subscribed)
    }

    /// Releases one reference per remote address, not per local destination.
    /// Disconnect and pool-shutdown errors are ignored during cleanup.
    pub(super) async fn unsubscribe(&self, keys: impl IntoIterator<Item = Pubkey>) {
        let pending = keys.into_iter().map(|key| self.websocket.unsubscribe(key));
        let _ = future::join_all(pending).await;
    }
}
