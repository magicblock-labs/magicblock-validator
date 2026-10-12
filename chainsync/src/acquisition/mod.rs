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
    AccountProperty::{Program, Readonly, Writable},
    AccountSubscription, ChainSync, ChainSyncAccount, Result, SyncTarget,
    metrics::{self, Op},
};

/// State shared by primary fetches, companion fetches, and action dependencies.
pub(super) struct Acquisition<'engine> {
    /// Supplies transports, normalization policy, and materialization into Engine.
    sync: &'engine ChainSync,
    /// References held for unfinished targets. Completed mirrors retain their own coverage.
    subs: BTreeSet<Pubkey>,
    /// Minimum response context slot, carried monotonically across dependency waves.
    floor: Option<u64>,
    /// HTTP entries fetched, including companions, nulls, and repeated primary fetches.
    count: u64,
    /// Readonly raw companions installed only after all target leases have been released.
    raw: BTreeMap<Pubkey, AccountBuilder>,
}

impl<'engine> Acquisition<'engine> {
    /// Starts with no account-selection policy; every input target is already leased.
    pub(super) fn new(sync: &'engine ChainSync, floor: Option<u64>) -> Self {
        Self {
            sync,
            subs: BTreeSet::new(),
            floor,
            count: 0,
            raw: BTreeMap::new(),
        }
    }

    /// Releases unfinished references on failure without retiring completed mirrors.
    pub(super) async fn run(mut self, targets: Vec<SyncTarget<'engine>>) -> Result<u64> {
        let _timer = metrics::time(Op::ChainSync);
        let result = self.waves(targets).await;
        self.sync.unsubscribe(self.subs).await;
        result.map(|()| self.count)
    }

    /// Resolves a whole wave before selecting action dependencies under new leases.
    async fn waves(&mut self, mut targets: Vec<SyncTarget<'engine>>) -> Result<()> {
        let mut deferred = Vec::new();
        while !targets.is_empty() {
            let pending = self.wave(targets).await?;
            let mut next = BTreeMap::new();
            let mut ready = Vec::new();
            for PendingDelegation { delegation, unsubscribe } in pending {
                let (prepared, dependencies) = self.sync.prepare_delegation(delegation)?;
                for ChainSyncAccount { pubkey, property } in dependencies {
                    // Preserve the companions required by either request for the same account.
                    let current = next.entry(pubkey).or_insert(property);
                    if property == Program || (*current == Readonly && property == Writable) {
                        *current = property;
                    }
                }
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
                target.subscribed = self.subs.contains(&target.accessor.pubkey());
            }
        }
        // Children must activate before the actions that requested their dependencies.
        for (prepared, unsubscribe) in deferred.into_iter().rev().flatten() {
            let pubkey = prepared.pubkey;
            self.sync.materialize_delegation(prepared).await?;
            if unsubscribe {
                self.sync.unsubscribe([pubkey]).await;
                self.subs.remove(&pubkey);
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
            let mut inputs = targets.into_iter().peekable();
            let mut next = Vec::new();
            let mut min_context_slot = None;
            while inputs.peek().is_some() {
                let outcome = self.batch(FetchPlan::next(&mut inputs)).await?;
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
            self.subs.remove(&pubkey);
        }
        Ok(pending)
    }

    /// Subscribes before fetching and installs only fully resolved account images.
    async fn batch(&mut self, plan: FetchPlan<'engine>) -> Result<BatchOutcome<'engine>> {
        let sync = self.sync;
        self.subs.extend(sync.subscribe(&plan.subscriptions).await?);
        let mut fetched = sync.fetcher.fetch(&plan.keys, self.floor).await?;
        self.count += plan.keys.len() as u64;
        let prune = prepare_programs(&plan, &mut fetched, sync.engine.rent())?;
        let projected = self.fetch_ata_companions(&plan, &fetched).await?;
        // Retire unused program subscriptions before transferring completed mirrors
        // out of cleanup ownership, including when a later target fails materialization.
        sync.unsubscribe(prune.iter().copied()).await;
        for pubkey in prune {
            self.subs.remove(&pubkey);
        }
        self.materialize_batch(plan.accounts, &mut fetched, projected).await
    }

    /// Stops failure cleanup from retiring a completed target's surviving mirror coverage.
    fn complete(&mut self, pubkey: Pubkey) {
        for sub in AccountSubscription::for_account(pubkey) {
            self.subs.remove(&sub.pubkey);
        }
    }
}

impl ChainSync {
    /// Waits for every request; partial failure releases only references acquired by this batch.
    async fn subscribe(
        &self,
        subscriptions: &[(AccountSubscription, bool)],
    ) -> Result<Vec<Pubkey>> {
        let requests =
            subscriptions.iter().map(|&(sub, reuse)| self.websocket.subscribe(sub, reuse));
        let mut subscribed = Vec::with_capacity(subscriptions.len());
        let mut failure = None;
        // Waiting for all results prevents a successful parallel request from going untracked.
        for ((subscription, _), result) in
            subscriptions.iter().zip(future::join_all(requests).await)
        {
            match result {
                Ok(true) => subscribed.push(subscription.pubkey),
                Ok(false) => {}
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
