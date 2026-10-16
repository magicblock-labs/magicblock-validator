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
    plan::{FetchPlan, PendingAccount},
};
use crate::{
    AccountProperty::{Payer, Program, Readonly, Writable},
    Result, SyncTarget,
    account::mirror,
    ata,
    delegation::{self, Delegation},
    http::FetchedAccounts,
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
        for chunk in atas.chunks(50) {
            let mut keys = Vec::with_capacity(chunk.len() * 2);
            for &(_, eata, _) in chunk {
                keys.push(eata);
                keys.push(delegation_record_pda_from_delegated_account(&eata));
            }
            let companions = self.sync.fetcher.fetch(&keys, Some(fetched.context_slot)).await?;
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
        mut account: AccountBuilder,
        fetched: &FetchedAccounts,
        projection: Option<Delegation>,
        outcome: &mut BatchOutcome<'engine>,
    ) -> Result<()> {
        let PendingAccount { mut target, index } = pending;
        let pubkey = target.accessor.pubkey();
        let projected = projection.is_some();
        // HTTP records stay borrowed; only projected records already own their bytes.
        let record = if let Some(delegation) = projection {
            account = delegation.account;
            Some((delegation.source_program, Cow::Owned(delegation.record)))
        } else if matches!(target.property, Payer | Writable)
            && let Some((metadata, record)) = delegation::accepted_record(
                &account,
                fetched.accounts[index + 1].as_ref(),
                self.sync.engine.authority(),
            )
        {
            account = if metadata.owner == EATA_PROGRAM_ID && ata::is_raw_eata(pubkey, &account) {
                ata::normalize_raw(account)
            } else {
                delegation::account(account, metadata)
            };
            Some((metadata.owner, Cow::Borrowed(record)))
        } else {
            None
        };
        if record.is_none() {
            let image = account.read();
            let executable = image.flags().contains(StateFlags::EXECUTABLE);
            let refetch = match target.property {
                Readonly if image.owner() == delegation_program_api::id() => Some(Writable),
                Readonly if executable && image.owner() == bpf_loader_upgradeable::ID => {
                    Some(Program)
                }
                Writable if executable => Some(Program),
                // Readonly discovery already subscribed: rejected delegations must not loop.
                Writable if !target.subscribed => Some(Readonly),
                _ => None,
            };
            if let Some(property) = refetch {
                // Keep companion reads at least as fresh as the response that discovered them,
                // independently of any delegation slot restored during normalization.
                outcome.min_context_slot = Some(fetched.context_slot);
                target.property = property;
                outcome.refetches.push(target);
                return Ok(());
            }
            // Incomplete images stay out of Engine until companions or tracking are acquired.
            // Readonly program discovery must not leave raw loader layouts in the cache.
            if target.property == Readonly && executable {
                account = program::normalize(account, None, self.sync.engine.rent())?;
            }
            if account.read().is(AccountMode::Uninit) {
                account = mirror(account);
            }
        }
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
        target.accessor.materialize(account, None).await?;
        if unsubscribe {
            self.sync.unsubscribe([pubkey]).await;
        }
        self.complete(pubkey);
        Ok(())
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
        if pending.target.property != Program {
            continue;
        }
        let program_index = pending.index;
        let data_index = program_index + 1;
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
