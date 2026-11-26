//! Resolves HTTP fetches and ATA companions, then materializes complete account images.

use std::borrow::Cow;

use delegation_program_api::pda::delegation_record_pda_from_delegated_account;
use mbv_core::token_programs::EATA_PROGRAM_ID;
use solana_account::{AccountBuilder, AccountMode, StateFlags};
use solana_pubkey::Pubkey;
use solana_rent::Rent;
use solana_sdk_ids::bpf_loader_upgradeable;

use super::{
    Acquisition,
    plan::{Companion, FetchPlan, PendingAccount},
};
use crate::{
    AccountProperty,
    AccountProperty::{Program, Readonly, Writable},
    Result, SyncTarget,
    account::mirror,
    ata,
    delegation::{self, Delegation},
    http::{BATCH_LIMIT, BatchKind, FetchedAccounts},
    program,
};

impl<'engine> Acquisition<'engine> {
    /// Resolves projections and stages their readonly raw eATAs without subscribing.
    /// Projections preserve primary-account order for matching during materialization.
    pub(super) async fn fetch_ata_companions(
        &mut self,
        plan: &FetchPlan<'_>,
        fetched: &FetchedAccounts,
    ) -> Result<Vec<Delegation>> {
        let mut atas = Vec::new();
        for pending in &plan.accounts {
            let Some(base) = fetched.accounts[pending.index].as_ref() else { continue };
            let pubkey = pending.target.accessor.pubkey();
            let Some(eata) = ata::companion(pubkey, base) else { continue };
            atas.push((pubkey, eata, base));
        }
        let mut projected = Vec::new();
        let authority = self.sync.engine.authority();
        for chunk in atas.chunks(BATCH_LIMIT / 2) {
            let mut keys = Vec::with_capacity(chunk.len() * 2);
            for &(_, eata, _) in chunk {
                keys.push(eata);
                keys.push(delegation_record_pda_from_delegated_account(&eata));
            }
            let companions = self
                .sync
                .fetcher
                .fetch(BatchKind::AtaCompanion, &keys, Some(fetched.context_slot))
                .await?;
            self.count += keys.len() as u64;
            for (&(pubkey, eata, base), pair) in
                chunk.iter().zip(companions.accounts.chunks_exact(2))
            {
                let Some(delegated) = pair[0].as_ref() else {
                    continue;
                };
                let Some((metadata, record)) =
                    delegation::accepted_record(delegated, pair[1].as_ref(), authority)
                else {
                    continue;
                };
                let Some(account) =
                    ata::project(pubkey, base.clone(), eata, delegated, record, authority)
                else {
                    continue;
                };
                let raw =
                    self.raw.entry(eata).or_insert_with(|| ata::normalize_raw(delegated.clone()));
                if delegated.read().slot() > raw.read().slot() {
                    *raw = ata::normalize_raw(delegated.clone());
                }
                projected.push(Delegation {
                    pubkey,
                    account,
                    source_program: metadata.owner,
                    record: record.to_vec(),
                });
            }
        }
        Ok(projected)
    }

    /// Materializes fetched images, deferring accounts that need companions or action dependencies.
    pub(super) async fn materialize_batch(
        &mut self,
        accounts: Vec<PendingAccount<'engine>>,
        fetched: &mut FetchedAccounts,
        projected: Vec<Delegation>,
    ) -> Result<BatchOutcome<'engine>> {
        let mut outcome = BatchOutcome::default();
        let mut projected = projected.into_iter().peekable();
        for pending in accounts {
            let pubkey = pending.target.accessor.pubkey();
            // Absence is an observation at this response context slot, not a slot-zero image.
            let account = fetched.accounts[pending.index]
                .take()
                .unwrap_or_else(|| AccountBuilder::default().slot(fetched.context_slot));
            let projection = projected.next_if(|delegation| delegation.pubkey == pubkey);
            self.materialize_account(pending, account, fetched, projection, &mut outcome)
                .await?;
        }
        Ok(outcome)
    }

    /// Keeps incomplete images out of Engine and defers activation until dependencies are ready.
    async fn materialize_account(
        &mut self,
        pending: PendingAccount<'engine>,
        account: AccountBuilder,
        fetched: &FetchedAccounts,
        projection: Option<Delegation>,
        outcome: &mut BatchOutcome<'engine>,
    ) -> Result<()> {
        let projected = projection.is_some();
        let resolution = pending.resolve(
            account,
            fetched,
            projection,
            self.sync.engine.authority(),
            &self.sync.engine.rent(),
        )?;
        let mut target = pending.target;
        let pubkey = target.accessor.pubkey();
        let (account, record) = match resolution {
            Resolution::Refetch(property) => {
                // Preserve the freshness of the response that revealed the companion.
                outcome.min_context_slot = Some(fetched.context_slot);
                target.property = property;
                outcome.refetches.push(target);
                return Ok(());
            }
            Resolution::Ready { account, record } => (account, record),
        };
        let unsubscribe = record.is_some() && (projected || target.subscribed);
        // Confined images never execute post-delegation actions.
        if account.read().is(AccountMode::Delegated)
            && let Some((source_program, record)) = record
            && delegation::appended(&record).is_some_and(|actions| !actions.is_empty())
        {
            let delegation = Delegation {
                pubkey,
                account,
                record: record.into_owned(),
                source_program,
            };
            outcome.delegations.push(PendingDelegation { delegation, unsubscribe });
            return Ok(());
        }
        if projected {
            ata::materialize(&self.sync.engine, target.accessor, account, None).await?;
        } else if account.read().is(AccountMode::Missing) {
            crate::account::materialize_missing(&self.sync.engine, target.accessor, account)
                .await?;
        } else {
            target.accessor.materialize(account, None).await?;
        }
        if unsubscribe {
            self.sync.unsubscribe([pubkey]).await;
        }
        self.complete(pubkey);
        Ok(())
    }
}

/// Image resolution under the target's existing lease, before any Engine mutation.
enum Resolution<'a> {
    /// Fetch the primary again with this role's companion and subscription requirements.
    Refetch(AccountProperty),
    /// Companion resolution is complete; delegation actions may still need dependencies.
    Ready {
        /// Normalized mirror, absence, delegation, or ATA projection.
        account: AccountBuilder,
        /// Original owning program and full delegation record, if accepted.
        /// For ATA projections the program is eSPL, not the ATA's token program.
        /// HTTP records borrow response data; projections already own their record bytes.
        record: Option<(Pubkey, Cow<'a, [u8]>)>,
    },
}

impl PendingAccount<'_> {
    /// Resolves an image without applying it or changing subscription ownership.
    fn resolve<'a>(
        &self,
        mut account: AccountBuilder,
        fetched: &'a FetchedAccounts,
        projection: Option<Delegation>,
        authority: Pubkey,
        rent: &Rent,
    ) -> Result<Resolution<'a>> {
        let target = &self.target;
        let pubkey = target.accessor.pubkey();
        if let Some(delegation) = projection {
            return Ok(Resolution::Ready {
                account: delegation.account,
                record: Some((delegation.source_program, Cow::Owned(delegation.record))),
            });
        }
        let record_account = match self.companion {
            Some(Companion::Record(index)) => fetched.accounts[index].as_ref(),
            _ => None,
        };
        if let Some((metadata, record)) =
            delegation::accepted_record(&account, record_account, authority)
        {
            account = if metadata.owner == EATA_PROGRAM_ID && ata::is_raw_eata(pubkey, &account) {
                ata::normalize_raw(account)
            } else {
                delegation::account(account, metadata)
            };
            return Ok(Resolution::Ready {
                account,
                record: Some((metadata.owner, Cow::Borrowed(record))),
            });
        }
        let image = account.read();
        let executable = image.flags().contains(StateFlags::EXECUTABLE);
        let refetch = match target.property {
            Readonly if image.owner() == delegation_program_api::id() => Some(Writable),
            Readonly if executable && image.owner() == bpf_loader_upgradeable::ID => Some(Program),
            Writable if executable => Some(Program),
            // Readonly discovery already subscribed: rejected delegations must not loop.
            Writable if !target.subscribed => Some(Readonly),
            _ => None,
        };
        if let Some(property) = refetch {
            return Ok(Resolution::Refetch(property));
        }
        // Incomplete images stay out of Engine until companions or tracking are acquired.
        // Readonly program discovery must not leave raw loader layouts in the cache.
        if target.property == Readonly && executable {
            account = program::normalize(account, None, rent)?;
        }
        if account.read().is(AccountMode::Missing) {
            account = mirror(account);
        }
        Ok(Resolution::Ready { account, record: None })
    }
}

/// Delegated account awaiting action dependencies after its initial lease is released.
pub(super) struct PendingDelegation {
    /// Normalized account and record payload retained without an Engine lease.
    pub(super) delegation: Delegation,
    /// Whether to release the primary subscription reference after materialization succeeds.
    pub(super) unsubscribe: bool,
}

/// Unresolved work from one batch: companions retain leases; action dependencies do not.
#[derive(Default)]
pub(super) struct BatchOutcome<'engine> {
    /// Same leased targets with a new role requiring another companion fetch.
    pub(super) refetches: Vec<SyncTarget<'engine>>,
    /// Highest response context slot among accounts needing another fetch.
    pub(super) min_context_slot: Option<u64>,
    /// Delegated accounts whose action dependencies must resolve before materialization.
    pub(super) delegations: Vec<PendingDelegation>,
}

/// Prepares only Program requests; Readonly loader discovery happens during materialization.
/// Returns unused subscription keys: the primary for V3, ProgramData for other loaders.
pub(super) fn prepare_programs(
    plan: &FetchPlan<'_>,
    fetched: &mut FetchedAccounts,
    rent: &Rent,
) -> Result<Vec<Pubkey>> {
    let mut prune = Vec::new();
    for pending in &plan.accounts {
        let Some(Companion::ProgramData(data_index)) = pending.companion else {
            continue;
        };
        let program_index = pending.index;
        let Some(program) = fetched.accounts[program_index].take() else { continue };
        let index = if program.read().owner() == bpf_loader_upgradeable::ID {
            program_index
        } else {
            data_index
        };
        prune.push(plan.keys[index]);
        let program_data = fetched.accounts[data_index].take();
        fetched.accounts[program_index] = Some(program::normalize(program, program_data, rent)?);
    }
    Ok(prune)
}
