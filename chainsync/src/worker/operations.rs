//! Account work never borrows coverage: checks and retirement go through the responsive worker.

use engine::EngineError;
use solana_account::{AccountBuilder, StateFlags};
use solana_pubkey::Pubkey;
use tokio::sync::{mpsc::UnboundedSender, oneshot};
use tracing::error;

use super::{
    Control,
    coverage::{Ticket, UpdateSource},
};
use crate::{
    AccountProperty, ChainSync, Result, SyncTarget,
    account::mirror,
    delegation::Delegation,
    metrics::{self, Op},
    program,
};

/// One logical operation occupies one scheduler slot until definitive completion or shutdown.
pub(super) enum Work {
    Update {
        source: UpdateSource,
        ticket: Ticket,
        update: Update,
    },
    Delegated(Delegation),
    UndelegationRequested {
        pubkey: Pubkey,
        slot: u64,
    },
    Undelegated {
        pubkey: Pubkey,
        slot: u64,
    },
    Evict {
        pubkey: Pubkey,
        cached: bool,
    },
}

/// Ordinary images stay in their account lane; raw eATA observations require multi-account acquisition.
pub(super) enum Update {
    Mirror(AccountBuilder),
    Refresh(u64),
}

impl Work {
    /// Multi-account lifecycle and raw-eATA acquisition are barriers; other work has one local lane.
    pub(super) fn lane(&self) -> Option<Pubkey> {
        match self {
            Self::Update {
                ticket,
                update: Update::Mirror(_),
                ..
            } => Some(ticket.sub.local_pubkey()),
            Self::Evict { pubkey, .. } => Some(*pubkey),
            Self::Update { update: Update::Refresh(_), .. }
            | Self::Delegated(_)
            | Self::UndelegationRequested { .. }
            | Self::Undelegated { .. } => None,
        }
    }
}

/// The lane remains occupied even while an operation awaits worker-owned control state.
pub(super) struct Completion {
    pub(super) lane: Option<Pubkey>,
    pub(super) result: Result<()>,
    pub(super) fatal: bool,
}

/// Shared services plus a private control path, with at most one outstanding request per operation.
pub(super) struct Operations<'a> {
    pub(super) sync: &'a ChainSync,
    pub(super) controls: UnboundedSender<Control>,
}

impl Operations<'_> {
    pub(super) async fn execute(&self, work: Work) -> Completion {
        let lane = work.lane();
        let fatal = matches!(work, Work::Evict { .. });
        let (pubkey, message, provider, result) = match work {
            Work::Update { source, ticket, update } => {
                let (message, provider) = match source {
                    UpdateSource::WebSocket => ("WS update failed", None),
                    UpdateSource::Grpc(stream) => {
                        ("gRPC update failed", self.sync.grpc[stream].hostname())
                    }
                };
                (
                    ticket.sub.pubkey,
                    message,
                    provider,
                    self.apply(ticket, update).await,
                )
            }
            Work::Delegated(delegation) => {
                let pubkey = delegation.pubkey;
                (
                    pubkey,
                    "delegation failed",
                    None,
                    self.delegated(delegation).await,
                )
            }
            Work::UndelegationRequested { pubkey, slot } => (
                pubkey,
                "undelegation scheduling failed",
                None,
                self.undelegation_requested(pubkey, slot).await,
            ),
            Work::Undelegated { pubkey, slot } => (
                pubkey,
                "undelegation failed",
                None,
                self.undelegated(pubkey, slot).await,
            ),
            Work::Evict { pubkey, cached } => (
                pubkey,
                "eviction failed",
                None,
                self.evict(pubkey, cached).await,
            ),
        };
        if let Err(error) = &result {
            error!(provider, %pubkey, %error, "{message}");
        }
        Completion { lane, result, fatal }
    }

    async fn control<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T>>) -> Control,
    ) -> Result<T> {
        let (reply, result) = oneshot::channel();
        self.controls.send(make(reply)).map_err(|_| EngineError::ShuttingDown)?;
        result.await.map_err(|_| EngineError::ShuttingDown)?
    }

    /// Retirement is acknowledged only after coverage is revoked and transport removal is queued.
    pub(super) async fn unsubscribe_account(&self, pubkey: Pubkey) -> Result<()> {
        self.control(|reply| Control::Retire { pubkey, reply }).await
    }

    /// Recheck eligibility under the Engine lease, including coverage restored while waiting for it.
    async fn evict(&self, pubkey: Pubkey, cached: bool) -> Result<()> {
        let accessor = self.sync.engine.account(pubkey).await?;
        let accessor =
            if cached { accessor.into_cached_eviction() } else { accessor.into_eviction() };
        let Some(accessor) = accessor else { return Ok(()) };
        if cached {
            // Keep the lease until removal is queued, before another caller can reacquire.
            self.unsubscribe_account(pubkey).await?;
        } else if self.control(|reply| Control::Covered { pubkey, reply }).await? {
            return Ok(());
        }
        accessor.delete().await?;
        Ok(())
    }

    /// Recheck the subscription generation after acquiring the lease.
    async fn apply(&self, ticket: Ticket, update: Update) -> Result<()> {
        let _timer = metrics::time(Op::Apply);
        let sub = ticket.sub;
        let accessor = self.sync.engine.account(sub.local_pubkey()).await?;
        if !self.control(|reply| Control::SubscriptionCurrent { ticket, reply }).await? {
            return Ok(());
        }
        let account = match update {
            Update::Mirror(account) => account,
            Update::Refresh(slot) => {
                let target = SyncTarget::new(accessor, AccountProperty::Readonly);
                self.sync.sync(vec![target], Some(slot)).await?;
                return Ok(());
            }
        };
        let account = match sub.program {
            Some(_) => program::normalize_program_data(account, &self.sync.engine.rent())?,
            None if account.read().flags().contains(StateFlags::EXECUTABLE) => {
                program::normalize(account, None, &self.sync.engine.rent())?
            }
            None => mirror(account),
        };
        accessor.materialize(account, None).await?;
        Ok(())
    }
}
