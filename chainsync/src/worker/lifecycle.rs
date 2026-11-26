use engine::{AccountAccessor, EngineError};
use solana_account::{AccountBuilder, AccountMode, AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
use tracing::warn;

use super::Operations;
use crate::{
    AccountProperty, ChainSyncAccount, Error, Result, SyncTarget, ata,
    delegation::{self, Delegation},
    metrics::{self, Op},
};

impl<'a> Operations<'a> {
    /// Activates a delegation only after retiring base-chain mirror coverage.
    pub(super) async fn delegated(&self, mut delegation: Delegation) -> Result<()> {
        // The eATA program also owns noncanonical shuttle accounts. Only a
        // validated canonical eATA is projected; other delegations activate normally.
        let image = delegation.account.read();
        if let Some(candidates) = ata::candidates(delegation.pubkey, image.owner(), image.data()) {
            self.unsubscribe_account(delegation.pubkey).await?;
            let Some(projection) = self.project_delegation(delegation, candidates).await? else {
                return Ok(());
            };
            delegation = projection;
        }
        // Confirmed ownership no longer permits mirroring, even if activation later fails.
        self.unsubscribe_account(delegation.pubkey).await?;
        if delegation.account.read().is(AccountMode::Magic) {
            // Confined accounts have no commit authority: no actions, dependencies, or rescue.
            self.sync
                .engine
                .account(delegation.pubkey)
                .await?
                .materialize(delegation.account, None)
                .await?;
            return Ok(());
        }
        let (prepared, dependencies) = self.sync.prepare_delegation(delegation)?;
        self.sync.ensure(dependencies).await?;
        self.sync.materialize_delegation(prepared).await
    }

    /// Projects onto a resident ATA, acquiring unknown ATAs only for action-bearing delegations.
    async fn project_delegation(
        &self,
        delegation: Delegation,
        candidates: [Pubkey; 2],
    ) -> Result<Option<Delegation>> {
        let Delegation {
            pubkey: eata,
            account: raw,
            record,
            source_program,
        } = delegation;
        let mut projection = None;
        for ata in candidates {
            let accessor = self.sync.engine.account(ata).await?;
            let Some(mut base) =
                self.read_account(ata, |account| AccountBuilder::from(account.clone()))?
            else {
                continue;
            };
            if base.read().is(AccountMode::Magic) {
                let fetched = self.sync.fetcher.fetch(&[ata], Some(raw.read().slot())).await?;
                let Some(remote) = fetched.accounts.into_iter().next().flatten() else {
                    continue;
                };
                base = remote;
            }
            let Some(account) =
                ata::project(ata, base, eata, &raw, &record, self.sync.engine.authority())
            else {
                continue;
            };
            if accessor.observed().is_some_and(|(_, slot)| account.read().slot() < slot) {
                continue;
            }
            drop(accessor);
            projection = Some((ata, account));
            break;
        }
        {
            let accessor = self.sync.engine.account(eata).await?;
            if projection.is_some() || accessor.exists() {
                accessor.materialize(ata::normalize_raw(raw), None).await?;
            }
        }
        if let Some((pubkey, account)) = projection {
            return Ok(Some(Delegation {
                pubkey,
                account,
                record,
                source_program,
            }));
        }
        if delegation::appended(&record).is_some_and(|actions| !actions.is_empty()) {
            // Shuttle actions must run even when nobody has requested their ATA yet.
            // Ordinary acquisition resolves the projection and activates its actions;
            // no lease is held here; acquisition also installs the readonly raw eATA.
            self.sync
                .ensure(candidates.map(|pubkey| ChainSyncAccount {
                    pubkey,
                    property: AccountProperty::Readonly,
                }))
                .await?;
        }
        Ok(None)
    }

    /// Reads local state under the caller's lease without letting a borrowed image escape.
    fn read_account<T>(
        &self,
        pubkey: Pubkey,
        reader: impl Fn(&AccountSharedData) -> T,
    ) -> Result<Option<T>> {
        self.sync
            .engine
            .accounts()
            .loader()
            .read(&pubkey, reader)
            .map_err(|error| EngineError::State(error.into()).into())
    }

    /// Returns a matching projection with its identity check protected by the returned lease.
    /// The lease borrows the shared Engine, not coverage, so the worker can retire tracking while held.
    async fn projection(
        &self,
        pubkey: Pubkey,
        eata: Pubkey,
    ) -> Result<Option<AccountAccessor<'a>>> {
        let accessor = self.sync.engine.account(pubkey).await?;
        let matches = self
            .read_account(pubkey, |account| {
                ata::is_projection_of(pubkey, eata, account)
            })?
            .unwrap_or(false);
        Ok(matches.then_some(accessor))
    }

    /// Keeps resident backing identity available even when completion closes the remote eATA.
    /// A resident non-eATA image is decisive too; only absence falls back to HTTP.
    async fn projection_candidates(
        &self,
        pubkey: Pubkey,
        slot: u64,
    ) -> Result<Option<[Pubkey; 2]>> {
        let resident = self.read_account(pubkey, |account| {
            ata::candidates(pubkey, *account.owner(), account.data())
        })?;
        if let Some(candidates) = resident {
            return Ok(candidates);
        }
        let fetched = self.sync.fetcher.fetch(&[pubkey], Some(slot)).await?;
        let Some(account) = fetched.accounts.into_iter().next().flatten() else {
            return Ok(None);
        };
        let image = account.read();
        Ok(ata::candidates(pubkey, image.owner(), image.data()))
    }

    /// Schedules current local state only for a request at or after its delegation slot.
    /// The serialized worker and resulting Transient mode suppress repeated observations;
    /// confirmed undelegation, not scheduling success, permits eventual cleanup.
    pub(super) async fn undelegation_requested(&self, pubkey: Pubkey, slot: u64) -> Result<()> {
        let accessor = self.sync.engine.account(pubkey).await?;
        if let Some(candidates) = self.projection_candidates(pubkey, slot).await? {
            drop(accessor);
            for ata in candidates {
                if let Some(accessor) = self.projection(ata, pubkey).await? {
                    self.request_undelegation(accessor, slot).await?;
                }
            }
            return Ok(());
        }
        self.request_undelegation(accessor, slot).await
    }

    /// Uses only the live delegated image; readonly raw eATAs are never scheduled.
    async fn request_undelegation(&self, accessor: AccountAccessor<'_>, slot: u64) -> Result<()> {
        let Some((AccountMode::Delegated, delegated_at)) = accessor.observed() else {
            return Ok(());
        };
        if slot < delegated_at {
            return Ok(());
        }
        let action =
            delegation::undelegation_action(self.sync.engine.authority(), accessor.pubkey());
        // Hold the lease through completion to serialize ChainSync lifecycle mutations.
        self.sync
            .engine
            .transaction(&[action])?
            .execute()
            .await?
            .map_err(EngineError::from)?;
        drop(accessor);
        Ok(())
    }

    /// Refreshes an eligible local account or its projected ATAs after confirmed undelegation.
    pub(super) async fn undelegated(&self, pubkey: Pubkey, slot: u64) -> Result<()> {
        let _timer = metrics::time(Op::Undelegate);
        let accessor = self.sync.engine.account(pubkey).await?;
        // Capture the backing identity before HTTP refresh can observe its closure.
        let candidates = self.projection_candidates(pubkey, slot).await?;
        if !accessor.exists() && candidates.is_none() {
            return Ok(());
        }
        let mut result = self.refresh_undelegated(accessor, slot).await;
        // Each refresh consumes its own lease. A failed raw refresh must not
        // prevent reconciliation of the independently protected projection.
        for ata in candidates.into_iter().flatten() {
            let Some(accessor) = self.projection(ata, pubkey).await? else { continue };
            let refreshed = self.refresh_undelegated(accessor, slot).await;
            result = result.and(refreshed);
        }
        result
    }

    /// Retains the current image until Readonly acquisition resolves a fresh account image.
    /// Fetch or subscription failures fall back to deletion after rechecking under a new lease.
    async fn refresh_undelegated(&self, accessor: AccountAccessor<'_>, slot: u64) -> Result<()> {
        if accessor.exists() && !can_apply_undelegation(&accessor, slot) {
            return Ok(());
        }
        let pubkey = accessor.pubkey();
        let target = SyncTarget::new(accessor, AccountProperty::Readonly);
        let result = self.sync.sync(vec![target], Some(slot)).await;
        let Err(error @ (Error::Fetch(_) | Error::Subscribe(_))) = &result else {
            return result.map(|_| ());
        };
        warn!(%pubkey, slot, %error, "undelegation refresh failed; deleting eligible account");
        // Synchronization consumed the original lease. Recheck the current
        // observation before fallback deletion, not the pre-fetch state.
        let accessor = self.sync.engine.account(pubkey).await?;
        if can_apply_undelegation(&accessor, slot) {
            self.unsubscribe_account(pubkey).await?;
            accessor.delete().await?;
        }
        result.map(|_| ())
    }
}

/// A confirmed undelegation can affect a present mirror or Transient account
/// only if the event is not older than the lease's observation.
fn can_apply_undelegation(accessor: &AccountAccessor<'_>, slot: u64) -> bool {
    let Some((mode, observed)) = accessor.observed() else { return false };
    let replaceable = !mode.requires_persistence() || mode == AccountMode::Undelegating;
    observed <= slot && replaceable
}
