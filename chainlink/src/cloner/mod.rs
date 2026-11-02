use std::iter;

use engine::{AccountAccessor, Engine, EngineError, PostFinalize};
use errors::{ClonerError, ClonerResult};
use keeper::error::KeeperError;
use magic_program_api::{
    MAGIC_CONTEXT_PUBKEY,
    args::{CommitAndUndelegateArgs, CommitTypeArgs, MagicIntentBundleArgs, UndelegateTypeArgs},
    instruction::MagicBlockInstruction,
};
use mbv_core::token_programs::{is_ata, try_get_magic_ata_info};
use solana_account::{AccountBuilder, AccountMode, AccountSharedData};
use solana_instruction::{AccountMeta, Instruction};
use solana_loader_v4_interface::state::LoaderV4Status;
use solana_program::instruction::InstructionError;
use solana_pubkey::Pubkey;
use solana_transaction_error::TransactionError;
use tracing::{debug, warn};

use crate::remote_account_provider::program_account::{
    LOADER_V1, LOADER_V4, LoadedProgram, RemoteProgramLoader,
};

mod ata;
pub mod errors;

/// Non-empty post-delegation actions paired with their slot-matched owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DelegationActions {
    source_program: Pubkey,
    actions: Vec<Instruction>,
}

impl DelegationActions {
    /// Returns a provenance-bearing bundle, or `None` when there are no actions.
    pub(crate) fn new(source_program: Pubkey, actions: Vec<Instruction>) -> Option<Self> {
        (!actions.is_empty()).then_some(Self { source_program, actions })
    }

    /// Program that owned the delegated account at the matched base-layer slot.
    pub(crate) fn source_program(&self) -> Pubkey {
        self.source_program
    }

    /// Instructions to execute after account activation.
    pub(crate) fn actions(&self) -> &[Instruction] {
        &self.actions
    }

    /// Yields non-target program and account dependencies with their writability.
    pub(crate) fn dependencies(&self, target: Pubkey) -> impl Iterator<Item = (Pubkey, bool)> + '_ {
        self.actions
            .iter()
            .flat_map(|ix| {
                iter::once((ix.program_id, false))
                    .chain(ix.accounts.iter().map(|meta| (meta.pubkey, meta.is_writable)))
            })
            .filter(move |(pubkey, _)| *pubkey != target)
    }

    fn into_post_finalize(self) -> PostFinalize {
        PostFinalize {
            source_program: self.source_program,
            actions: self.actions,
        }
    }
}

/// Mutually exclusive post-delegation behavior for a clone request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ClonePostDelegationMode {
    /// Clone without post-delegation actions or rescue undelegation.
    #[default]
    None,
    /// Clone and execute post-delegation actions after activation.
    ExecuteActions(DelegationActions),
    /// Clone and schedule undelegation instead of executing actions.
    ///
    /// Used for delegated accounts whose post-delegation actions cannot be
    /// executed safely, for example because they include risky signers.
    RescueUndelegate(Pubkey),
}

impl ClonePostDelegationMode {
    /// Returns the action bundle when this request activates a delegation.
    pub(crate) fn delegation(&self) -> Option<&DelegationActions> {
        match self {
            Self::ExecuteActions(actions) => Some(actions),
            Self::None | Self::RescueUndelegate(_) => None,
        }
    }

    pub(crate) fn has_actions(&self) -> bool {
        self.delegation().is_some()
    }
}

impl From<Option<DelegationActions>> for ClonePostDelegationMode {
    fn from(actions: Option<DelegationActions>) -> Self {
        actions.map_or(Self::None, Self::ExecuteActions)
    }
}

/// An account snapshot and its activation work. Fetch freshness remains
/// separate from the stored delegation stamp.
pub struct AccountCloneRequest {
    pub pubkey: Pubkey,
    pub account: AccountBuilder,
    /// Trusted post-delegation state; kept private to prevent external callers
    /// from constructing requests with unverified invocation provenance.
    pub(crate) post_delegation_mode: ClonePostDelegationMode,
    /// If the account is delegated to another validator,
    /// this contains that validator's pubkey. None if account is not
    /// delegated to another validator.
    pub delegated_to_other: Option<Pubkey>,
    /// Input snapshots used to resolve a delegated account or ATA projection.
    /// `None` uses the account slot for both bounds.
    pub source_slots: Option<CloneSourceSlots>,
}

impl AccountCloneRequest {
    pub(crate) fn is_ata_projection(&self) -> bool {
        let account = self.account.read();
        account.is(AccountMode::Delegated)
            && is_ata(&self.pubkey, account.owner(), account.data()).is_some()
    }

    pub(crate) fn source_slots(&self) -> CloneSourceSlots {
        self.source_slots
            .unwrap_or_else(|| CloneSourceSlots::single(self.account.read().slot()))
    }
}

/// Source provenance, independent of the account's stored delegation stamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloneSourceSlots {
    /// Account-data snapshot; a newer plain local copy supersedes this input.
    pub data: u64,
    /// Freshest input, including companions; bounds action dependency freshness.
    pub view: u64,
}

impl CloneSourceSlots {
    pub fn single(slot: u64) -> Self {
        Self { data: slot, view: slot }
    }

    pub(crate) fn projected(ata: u64, eata: u64) -> Self {
        Self { data: ata, view: ata.max(eata) }
    }
}

/// Claims a remote image, protecting Magic state except validated ATA projections.
/// The lease serializes maintenance, not ordinary transaction execution.
pub(crate) async fn claim_materialization(
    engine: &Engine,
    pubkey: Pubkey,
    ata_projection: bool,
) -> ClonerResult<Option<AccountAccessor<'_>>> {
    let accessor = engine.account(pubkey).await?;
    if !matches!(accessor.observed(), Some((AccountMode::Magic, _))) {
        return Ok(Some(accessor));
    }
    if !ata_projection {
        return Ok(None);
    }
    let is_magic_ata = engine
        .accounts()
        .loader()
        .read(&pubkey, |account| {
            try_get_magic_ata_info(&pubkey, account).is_some()
        })
        .map_err(KeeperError::from)
        .map_err(EngineError::from)?
        .unwrap_or(false);
    Ok(is_magic_ata.then_some(accessor))
}

fn undelegation_action(engine: &Engine, pubkey: Pubkey) -> Instruction {
    let args = MagicIntentBundleArgs {
        commit_and_undelegate: Some(CommitAndUndelegateArgs {
            // Payer and Magic Context occupy action account indices 0 and 1.
            commit_type: CommitTypeArgs::Standalone(vec![2]),
            undelegate_type: UndelegateTypeArgs::Standalone,
        }),
        ..Default::default()
    };
    Instruction::new_with_wincode(
        magic_program_api::id(),
        &MagicBlockInstruction::ScheduleIntentBundle(args),
        vec![
            // MagicRoot vouches for declared action signers during native CPI.
            // Keep the authority readonly so its post-finalize mutability guard
            // does not reject the validator's immutable identity account.
            AccountMeta::new_readonly(engine.authority(), true),
            AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
            AccountMeta::new(pubkey, false),
        ],
    )
}

pub(crate) async fn clone_account(
    engine: &Engine,
    accessor: AccountAccessor<'_>,
    request: AccountCloneRequest,
) -> ClonerResult<()> {
    // Delegation was confirmed before ER execution could request undelegation.
    // Its completion must therefore advance that base-chain delegation slot.
    // Recheck under the lease in case another delegation won while we waited.
    // A queued transaction can still move Delegated to Undelegating afterward,
    // but that transition preserves the same delegation stamp.
    if let Some((AccountMode::Delegated | AccountMode::Undelegating, delegation_slot)) =
        accessor.observed()
    {
        let incoming = request.account.read();
        let completes_undelegation = incoming.is_any([AccountMode::Missing, AccountMode::Mirrored]);
        let advances_delegation = incoming.slot() > delegation_slot;
        if completes_undelegation && !advances_delegation {
            return Ok(());
        }
    }
    if let Some(authority) = request.delegated_to_other {
        warn!(
            pubkey = %request.pubkey,
            delegated_to = %authority,
            "Cloning account delegated to another validator"
        );
    }
    let ata_projection = request.is_ata_projection();
    let actions = match request.post_delegation_mode {
        ClonePostDelegationMode::None => None,
        ClonePostDelegationMode::ExecuteActions(actions) => Some(actions.into_post_finalize()),
        ClonePostDelegationMode::RescueUndelegate(source_program) => Some(PostFinalize {
            source_program,
            actions: vec![undelegation_action(engine, request.pubkey)],
        }),
    };
    let account = request.account;
    let empty_placeholder = account.read().is(AccountMode::Missing) && actions.is_none();
    let result = if ata_projection
        && accessor.skipped(account.read().mode(), account.read().slot()).is_none()
    {
        ata::materialize(engine, accessor, account, actions).await
    } else {
        accessor.materialize(account, actions).await.map_err(Into::into)
    };
    let error = match result {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    // A queued local creation can win after the original lease observed
    // absence. Reacquire before accepting its protected image as sufficient.
    const LIFECYCLE_CONFLICT: TransactionError =
        TransactionError::InstructionError(1, InstructionError::InvalidArgument);
    let lifecycle_conflict = matches!(
        error,
        ClonerError::Engine(EngineError::TransactionExecution(LIFECYCLE_CONFLICT))
    );
    if empty_placeholder && lifecycle_conflict {
        let locally_created = claim_materialization(engine, request.pubkey, false).await?.is_none();
        if locally_created {
            return Ok(());
        }
    }
    Err(ClonerError::FailedToCloneRegularAccount(
        request.pubkey,
        Box::new(error),
    ))
}

pub(crate) fn resolve_program(program: LoadedProgram) -> Option<AccountCloneRequest> {
    let program_id = program.program_id;
    if matches!(program.loader_status, LoaderV4Status::Retracted) {
        debug!(%program_id, "Program is retracted on chain");
        return None;
    }

    let owner = match program.loader {
        RemoteProgramLoader::V1 => LOADER_V1,
        RemoteProgramLoader::V2 | RemoteProgramLoader::V3 | RemoteProgramLoader::V4 => LOADER_V4,
    };
    let account = AccountBuilder::default()
        .lamports(program.lamports())
        .data(program.program_data)
        .owner(owner)
        .mode(AccountMode::Mirrored)
        .executable(true)
        .slot(program.remote_slot);

    Some(AccountCloneRequest {
        pubkey: program_id,
        account,
        post_delegation_mode: ClonePostDelegationMode::None,
        delegated_to_other: None,
        source_slots: None,
    })
}

pub(crate) async fn clone_program(
    accessor: AccountAccessor<'_>,
    request: AccountCloneRequest,
) -> ClonerResult<()> {
    let program_id = request.pubkey;
    accessor
        .materialize(request.account, None)
        .await
        .map_err(|err| ClonerError::FailedToCloneProgram(program_id, Box::new(err.into())))
}

pub(crate) async fn evict_account(engine: &Engine, pubkey: Pubkey) -> ClonerResult<()> {
    let Some(accessor) = claim_account_eviction(engine, pubkey).await? else {
        return Ok(());
    };
    delete_claimed_account(accessor, pubkey).await
}

pub(crate) async fn delete_claimed_account(
    accessor: AccountAccessor<'_>,
    pubkey: Pubkey,
) -> ClonerResult<()> {
    accessor
        .delete()
        .await
        .map_err(|err| ClonerError::FailedToEvictAccount(pubkey, Box::new(err.into())))
}

/// Claims an account displaced from Engine recency, unless a later completion
/// retained it again or changed it to an authoritative lifecycle mode. The
/// returned value is read under the accepted lease for ATA projection.
pub(crate) async fn claim_cached_account_eviction<R>(
    engine: &Engine,
    pubkey: Pubkey,
    inspect: impl Fn(&AccountSharedData) -> R,
) -> ClonerResult<Option<(AccountAccessor<'_>, R)>> {
    let accessor = engine.account(pubkey).await?;
    let Some(accessor) = accessor.into_cached_eviction() else {
        return Ok(None);
    };
    let value = engine
        .accounts()
        .loader()
        .read(&pubkey, inspect)
        .map_err(KeeperError::from)
        .map_err(EngineError::from)?;
    Ok(value.map(|value| (accessor, value)))
}

/// Claims a requested account eviction unless the current state is absent or
/// authoritative.
pub(crate) async fn claim_account_eviction(
    engine: &Engine,
    pubkey: Pubkey,
) -> ClonerResult<Option<AccountAccessor<'_>>> {
    Ok(engine.account(pubkey).await?.into_eviction())
}

#[cfg(test)]
mod tests {
    use engine::testkit::TestEngine;

    use super::*;

    /// Proves a queued cache eviction cannot claim an account after a later
    /// materialization retained it in recency or made it authoritative.
    #[tokio::test]
    async fn stale_cache_eviction_does_not_claim_current_account() {
        let engine = TestEngine::new().await;
        let pubkey = Pubkey::new_unique();
        engine
            .account(pubkey)
            .await
            .unwrap()
            .materialize(
                AccountBuilder::default().lamports(1_000_000).mode(AccountMode::Mirrored),
                None,
            )
            .await
            .expect("read-only account is materialized");

        assert!(
            claim_cached_account_eviction(&engine, pubkey, |_| ())
                .await
                .expect("eviction classification succeeds")
                .is_none(),
            "re-admission invalidates the queued eviction"
        );
        assert!(
            engine.get_account(pubkey).is_some(),
            "stale eviction leaves the re-admitted account intact"
        );

        let authoritative = Pubkey::new_unique();
        engine
            .account(authoritative)
            .await
            .unwrap()
            .materialize(
                AccountBuilder::default().lamports(1_000_000).mode(AccountMode::Magic),
                None,
            )
            .await
            .expect("ephemeral account is materialized");
        assert!(
            claim_cached_account_eviction(&engine, authoritative, |_| ())
                .await
                .expect("eviction classification succeeds")
                .is_none(),
            "authoritative state invalidates the queued eviction"
        );
        assert!(
            engine.get_account(authoritative).is_some(),
            "stale eviction leaves the authoritative account intact"
        );

        engine.close().await;
    }
}
