mod coverage;
mod lifecycle;

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::Arc,
};

use nucleus::shutdown::{ShutdownHandle, ShutdownReason};
use solana_account::{AccountBuilder, StateFlags};
use solana_pubkey::Pubkey;
use tokio::sync::mpsc::{Receiver, UnboundedReceiver};
use tracing::error;

use self::coverage::{Coverage, Source};
use crate::{
    AccountProperty, AccountSubscription, ChainSync, Result, SyncTarget,
    account::mirror,
    ata,
    grpc::{self, Command},
    metrics::{self, Op},
    program, websocket,
};

impl ChainSync {
    /// Always assigns a remote address to the same gRPC stream so its filter entry can be reused.
    fn grpc_client(&self, pubkey: Pubkey) -> &grpc::Client {
        let mut hash = DefaultHasher::new();
        pubkey.hash(&mut hash);
        &self.grpc[hash.finish() as usize % self.grpc.len()]
    }

    /// Runs the update worker until coordinated shutdown or a terminal processing failure.
    /// Drops the worker's shared handle before reporting its termination reason.
    pub(super) async fn run(
        self: Arc<Self>,
        websocket: Receiver<websocket::Event>,
        grpc: Receiver<grpc::Event>,
        evictions: UnboundedReceiver<Pubkey>,
        mut shutdown: ShutdownHandle,
    ) {
        let reason = tokio::select! {
            biased;
            _ = shutdown.signalled() => ShutdownReason::Signalled,
            result = self.run_updates(websocket, grpc, evictions) => match result {
                Ok(()) => {
                    error!("ChainSync worker stopped unexpectedly");
                    ShutdownReason::Unexpected
                }
                Err(error) => {
                    error!(%error, "ChainSync worker stopped unexpectedly");
                    ShutdownReason::Error(Box::new(error))
                }
            },
        };
        drop(self);
        shutdown.terminate(reason);
    }

    /// Owns coverage and serializes transport updates and evictions.
    /// Ends when Engine's eviction channel closes or an event handler fails.
    async fn run_updates(
        &self,
        mut websocket: Receiver<websocket::Event>,
        mut grpc: Receiver<grpc::Event>,
        mut evictions: UnboundedReceiver<Pubkey>,
    ) -> Result<()> {
        let mut coverage = Coverage::default();
        // Process coverage changes and updates in one loop so removal takes effect
        // before later buffered updates are checked and applied.
        loop {
            tokio::select! {
                biased;
                Some(event) = websocket.recv() => {
                    self.on_websocket(event, &mut coverage).await?;
                }
                Some(event) = grpc.recv() => {
                    self.on_grpc(event, &mut coverage).await?;
                }
                pubkey = evictions.recv() => {
                    let Some(pubkey) = pubkey else { return Ok(()) };
                    self.evict_cached(pubkey, &mut coverage).await?;
                }
            }
        }
    }

    /// Revokes local coverage and queues removal from all transports without waiting for providers.
    async fn unsubscribe_account(&self, pubkey: Pubkey, coverage: &mut Coverage) -> Result<()> {
        for sub in AccountSubscription::for_account(pubkey) {
            if coverage.remove(sub) {
                self.grpc_client(sub.pubkey).command(Command::Remove(sub.pubkey));
            }
        }
        self.websocket.unsubscribe_account(pubkey).await?;
        Ok(())
    }

    /// Deletes a cached mirror only if it is still eligible after acquiring its Engine lease.
    async fn evict_cached(&self, pubkey: Pubkey, coverage: &mut Coverage) -> Result<()> {
        let Some(accessor) = self.engine.account(pubkey).await?.into_cached_eviction() else {
            return Ok(());
        };
        // Hold the Engine lease until unsubscribe is queued, so reacquisition cannot
        // queue a new subscription before the old one is removed.
        self.unsubscribe_account(pubkey, coverage).await?;
        accessor.delete().await?;
        Ok(())
    }

    /// Applies WebSocket coverage changes and accepts images only from active subscriptions.
    /// Image failures are logged; failures handling coverage loss propagate to the worker.
    async fn on_websocket(&self, event: websocket::Event, coverage: &mut Coverage) -> Result<()> {
        match event {
            websocket::Event::Acknowledged(sub) => {
                let generation = coverage.acknowledged(sub);
                self.grpc_client(sub.pubkey).command(Command::Track { sub, generation });
            }
            websocket::Event::Removed(pubkey) => {
                // Explicit release only revokes coverage. Its caller owns local state;
                // delayed cleanup must not delete an account installed by reacquisition.
                coverage.removed(pubkey);
                self.grpc_client(pubkey).command(Command::Remove(pubkey));
            }
            websocket::Event::Update { sub, account } => {
                if coverage.ws_contains(sub)
                    && let Err(error) = self.apply(sub, account).await
                {
                    error!(%sub.pubkey, %error, "WS update failed");
                }
            }
            websocket::Event::Dropped { pubkeys } => {
                for pubkey in pubkeys {
                    self.lost(coverage, Source::WebSocket, pubkey).await?;
                }
            }
        }
        Ok(())
    }

    /// Drops one transport's coverage and removes the gRPC subscription if neither remains.
    async fn lost(&self, coverage: &mut Coverage, source: Source, pubkey: Pubkey) -> Result<()> {
        let (ended, local_pubkey) = coverage.lost(source, pubkey);
        if ended {
            self.grpc_client(pubkey).command(Command::Remove(pubkey));
        }
        if let Some(local_pubkey) = local_pubkey {
            self.evict(local_pubkey).await?;
        }
        Ok(())
    }

    /// Deletes a present Engine account only if it is still non-authoritative under its lease.
    async fn evict(&self, local_pubkey: Pubkey) -> Result<()> {
        if let Some(accessor) = self.engine.account(local_pubkey).await?.into_eviction() {
            accessor.delete().await?;
        }
        Ok(())
    }

    /// Applies gRPC coverage and delegation lifecycle events, rejecting uncovered ordinary updates.
    /// Image and lifecycle failures are logged; failures handling stream loss propagate.
    async fn on_grpc(&self, event: grpc::Event, coverage: &mut Coverage) -> Result<()> {
        match event {
            grpc::Event::Confirmed { stream, pubkey, generation } => {
                coverage.confirmed(stream, pubkey, generation);
            }
            grpc::Event::Lost(stream) => {
                for pubkey in coverage.stream_keys(stream) {
                    self.lost(coverage, Source::Grpc, pubkey).await?;
                }
            }
            grpc::Event::Update { stream, sub, account } => {
                if coverage.grpc_contains(stream, sub)
                    && let Err(error) = self.apply(sub, account).await
                {
                    let provider = self.grpc[stream].hostname();
                    error!(provider, %sub.pubkey, %error, "gRPC update failed");
                }
            }
            grpc::Event::Delegated(delegation) => {
                let pubkey = delegation.pubkey;
                if let Err(error) = self.delegated(delegation, coverage).await {
                    error!(%pubkey, %error, "delegation failed");
                }
            }
            grpc::Event::UndelegationRequested { pubkey, slot } => {
                if let Err(error) = self.undelegation_requested(pubkey, slot).await {
                    error!(%pubkey, slot, %error, "undelegation scheduling failed");
                }
            }
            grpc::Event::Undelegated { pubkeys, slot } => {
                for pubkey in pubkeys {
                    if let Err(error) = self.undelegated(pubkey, slot, coverage).await {
                        error!(%pubkey, slot, %error, "undelegation failed");
                    }
                }
            }
        }
        Ok(())
    }

    /// Materializes a remote update under its own address, or under the program for ProgramData.
    /// Ordinary updates are read-only while funded, uninitialized otherwise;
    /// delegation transitions are supplied by specialized gRPC events.
    async fn apply(
        &self,
        subscription: AccountSubscription,
        account: AccountBuilder,
    ) -> Result<()> {
        let _timer = metrics::time(Op::Apply);
        let AccountSubscription { pubkey, program } = subscription;
        if account.read().owner() == delegation_program_api::id()
            && ata::is_raw_eata(pubkey, &account)
        {
            let target = SyncTarget::new(
                self.engine.account(pubkey).await?,
                AccountProperty::Readonly,
            );
            return self.sync(vec![target], Some(account.read().slot())).await.map(|_| ());
        }
        let accessor = self.engine.account(subscription.local_pubkey()).await?;
        let account = match program {
            Some(_) => program::normalize_program_data(account, self.engine.rent())?,
            None if account.read().flags().contains(StateFlags::EXECUTABLE) => {
                program::normalize(account, None, self.engine.rent())?
            }
            None => mirror(account),
        };
        accessor.materialize(account, None).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
