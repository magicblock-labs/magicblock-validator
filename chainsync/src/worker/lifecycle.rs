use engine::{AccountAccessor, EngineError};
use solana_account::{AccountBuilder, AccountMode, AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
use tracing::warn;

use super::coverage::Coverage;
use crate::{
    AccountProperty, ChainSync, Error, Result, SyncTarget, ata,
    delegation::{self, Delegation},
    metrics::{self, Op},
};

impl ChainSync {
    /// Activates a delegation only after retiring base-chain mirror coverage.
    pub(super) async fn delegated(
        &self,
        mut delegation: Delegation,
        coverage: &mut Coverage,
    ) -> Result<()> {
        if ata::is_eata(&delegation.account) {
            let Some(projection) = self.project_delegation(delegation).await? else {
                return Ok(());
            };
            delegation = projection;
        }
        // Confirmed ownership no longer permits mirroring, even if activation later fails.
        self.unsubscribe_account(delegation.pubkey, coverage).await?;
        if delegation.account.read().is(AccountMode::Magic) {
            // Confined accounts have no commit authority: no actions, dependencies, or rescue.
            self.engine
                .account(delegation.pubkey)
                .await?
                .materialize(delegation.account, None)
                .await?;
            return Ok(());
        }
        let (prepared, dependencies) = self.prepare_delegation(delegation)?;
        self.ensure(dependencies).await?;
        self.materialize_delegation(prepared).await
    }

    /// Projects a gRPC eATA delegation onto a matching ATA already present in Engine.
    async fn project_delegation(&self, delegation: Delegation) -> Result<Option<Delegation>> {
        let Some(candidates) = ata::candidates(delegation.pubkey, &delegation.account) else {
            return Ok(None);
        };
        for ata in candidates {
            let accessor = self.engine.account(ata).await?;
            let Some(base) =
                self.read_account(ata, |account| AccountBuilder::from(account.clone()))?
            else {
                continue;
            };
            let Some(account) =
                ata::project(ata, base, eata, &raw, &record, self.engine.authority())
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
            let accessor = self.engine.account(eata).await?;
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
        Ok(None)
    }

    /// Reads local state under the caller's lease without letting a borrowed image escape.
    fn read_account<T>(
        &self,
        pubkey: Pubkey,
        reader: impl Fn(&AccountSharedData) -> T,
    ) -> Result<Option<T>> {
        self.engine
            .accounts()
            .loader()
            .read(&pubkey, reader)
            .map_err(|error| EngineError::State(error.into()).into())
    }

    /// Returns a matching projection with its identity check protected by the returned lease.
    async fn projection(
        &self,
        pubkey: Pubkey,
        eata: Pubkey,
    ) -> Result<Option<AccountAccessor<'_>>> {
        let accessor = self.engine.account(pubkey).await?;
        let matches = self
            .read_account(pubkey, |account| {
                ata::is_projection_of(pubkey, eata, account)
            })?
            .unwrap_or(false);
        Ok(matches.then_some(accessor))
    }

    /// Keeps resident backing identity available even when completion closes the remote eATA.
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
        let fetched = self.fetcher.fetch(&[pubkey], Some(slot)).await?;
        Ok(
            fetched.accounts.into_iter().next().flatten().and_then(|account| {
                let image = account.read();
                ata::candidates(pubkey, image.owner(), image.data())
            }),
        )
    }

    /// Schedules current local state only for a request at or after its delegation slot.
    /// The serialized worker and resulting Transient mode suppress repeated observations;
    /// confirmed undelegation, not scheduling success, permits eventual cleanup.
    pub(super) async fn undelegation_requested(&self, pubkey: Pubkey, slot: u64) -> Result<()> {
        let accessor = self.engine.account(pubkey).await?;
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
        if !matches!(accessor.observed(), Some((AccountMode::Delegated, at)) if slot >= at) {
            return Ok(());
        }
        let action = delegation::undelegation_action(self.engine.authority(), accessor.pubkey());
        // Hold the lease through completion to serialize ChainSync lifecycle mutations.
        self.engine
            .transaction(&[action])?
            .execute()
            .await?
            .map_err(EngineError::from)?;
        drop(accessor);
        Ok(())
    }

    /// Refreshes an eligible local account or its projected ATAs after confirmed undelegation.
    pub(super) async fn undelegated(
        &self,
        pubkey: Pubkey,
        slot: u64,
        coverage: &mut Coverage,
    ) -> Result<()> {
        let _timer = metrics::time(Op::Undelegate);
        let accessor = self.engine.account(pubkey).await?;
        // Capture the backing identity before HTTP refresh can observe its closure.
        let candidates = self.projection_candidates(pubkey, slot).await?;
        if !accessor.exists() && candidates.is_none() {
            return Ok(());
        }
        let mut result = self.refresh_undelegated(accessor, slot, coverage).await;
        // Each refresh consumes its own lease. A failed raw refresh must not
        // prevent reconciliation of the independently protected projection.
        for ata in candidates.into_iter().flatten() {
            let Some(accessor) = self.projection(ata, pubkey).await? else { continue };
            let refreshed = self.refresh_undelegated(accessor, slot, coverage).await;
            result = result.and(refreshed);
        }
        result
    }

    /// Retains the current image until Readonly acquisition resolves a fresh account image.
    /// HTTP failures fall back to deletion only after rechecking the lifecycle under a new lease.
    async fn refresh_undelegated(
        &self,
        accessor: AccountAccessor<'_>,
        slot: u64,
        coverage: &mut Coverage,
    ) -> Result<()> {
        if accessor.exists() && !can_apply_undelegation(&accessor, slot) {
            return Ok(());
        }
        let pubkey = accessor.pubkey();
        let target = SyncTarget::new(accessor, AccountProperty::Readonly);
        let result = self.sync(vec![target], Some(slot)).await;
        if let Err(Error::Fetch(error)) = &result {
            warn!(%pubkey, slot, %error, "undelegation refresh failed; deleting eligible account");
            // Synchronization consumed the original lease. Recheck the current
            // observation before fallback deletion, not the pre-fetch state.
            let accessor = self.engine.account(pubkey).await?;
            if can_apply_undelegation(&accessor, slot) {
                self.unsubscribe_account(pubkey, coverage).await?;
                accessor.delete().await?;
            }
        }
        result.map(|_| ())
    }
}

/// A confirmed undelegation can affect a present mirror or Transient account
/// only if the event is not older than the lease's observation.
fn can_apply_undelegation(accessor: &AccountAccessor<'_>, slot: u64) -> bool {
    matches!(accessor.observed(), Some((mode, observed))
        if observed <= slot && (!mode.authoritative() || mode == AccountMode::Transient))
}
