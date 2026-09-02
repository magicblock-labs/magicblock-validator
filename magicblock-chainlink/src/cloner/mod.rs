use std::{iter, mem};

use engine::{AccountAccessor, Engine, PostFinalize};
use errors::ClonerResult;
use magicblock_magic_program_api::{
    MAGIC_CONTEXT_PUBKEY,
    args::{
        CommitAndUndelegateArgs, CommitTypeArgs, MagicIntentBundleArgs,
        UndelegateTypeArgs,
    },
    instruction::MagicBlockInstruction,
};
use solana_account::{
    AccountBuilder, AccountMode, AccountSharedData, OwnedAccount,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_loader_v4_interface::state::LoaderV4Status;
use solana_pubkey::Pubkey;
use tracing::{debug, warn};

use crate::remote_account_provider::program_account::{
    LOADER_V1, LOADER_V4, LoadedProgram, RemoteProgramLoader,
};

pub mod errors;

/// Non-empty post-delegation actions paired with their slot-matched owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DelegationActions {
    source_program: Pubkey,
    actions: Vec<Instruction>,
}

impl DelegationActions {
    /// Returns a provenance-bearing bundle, or `None` when there are no actions.
    pub(crate) fn new(
        source_program: Pubkey,
        actions: Vec<Instruction>,
    ) -> Option<Self> {
        (!actions.is_empty()).then_some(Self {
            source_program,
            actions,
        })
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
    pub(crate) fn dependencies(
        &self,
        target: Pubkey,
    ) -> impl Iterator<Item = (Pubkey, bool)> + '_ {
        self.actions
            .iter()
            .flat_map(|ix| {
                iter::once((ix.program_id, false)).chain(
                    ix.accounts
                        .iter()
                        .map(|meta| (meta.pubkey, meta.is_writable)),
                )
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

pub struct AccountCloneRequest {
    pub pubkey: Pubkey,
    pub account: AccountBuilder,
    pub commit_frequency_ms: Option<u64>,
    /// Trusted post-delegation state; kept private to prevent external callers
    /// from constructing requests with unverified invocation provenance.
    pub(crate) post_delegation_mode: ClonePostDelegationMode,
    /// If the account is delegated to another validator,
    /// this contains that validator's pubkey. None if account is not
    /// delegated to another validator.
    pub delegated_to_other: Option<Pubkey>,
}

enum Materialization {
    Apply,
    ApplyReadOnly,
    Satisfied(AccountMode),
}

fn classify_materialization(
    pubkey: Pubkey,
    local: &AccountSharedData,
    desired: &OwnedAccount,
) -> ClonerResult<Materialization> {
    let invalid = |reason| {
        errors::ClonerError::InvalidAccountMaterialization(pubkey, reason)
    };
    let mode = local.mode();
    let active_delegation = mode == AccountMode::Delegated
        && desired.mode() == AccountMode::Delegated;
    if mode == AccountMode::Ephemeral
        || active_delegation
        || local.slot() > desired.slot()
    {
        return Ok(Materialization::Satisfied(mode));
    }
    if local == desired {
        return Ok(Materialization::Satisfied(mode));
    }

    let desired_mode = desired.mode();
    if mode == AccountMode::Transient
        && desired_mode == AccountMode::Placeholder
    {
        // Engine completes undelegation through Transient -> ReadOnly. A
        // zero-lamport remote image is still immutable, but cannot return
        // directly to Placeholder through the lifecycle state machine.
        return Ok(Materialization::ApplyReadOnly);
    }
    if local.slot() == desired.slot() {
        if mode == desired_mode {
            return Err(invalid("conflicting images at the same slot".into()));
        }
        if !mode.allows_transition(desired_mode, local.slot(), desired.slot()) {
            return Err(invalid(format!(
                "invalid same-slot mode transition {mode:?} -> {desired_mode:?}"
            )));
        }
        return Ok(Materialization::Apply);
    }

    if mode != desired_mode
        && !mode.allows_transition(desired_mode, local.slot(), desired.slot())
    {
        return Err(invalid(format!(
            "invalid mode transition {mode:?} -> {desired_mode:?}"
        )));
    }
    Ok(Materialization::Apply)
}

pub(crate) async fn claim_materialization<'a>(
    engine: &'a Engine,
    request: &mut AccountCloneRequest,
) -> ClonerResult<Option<AccountAccessor<'a>>> {
    let accessor = engine.account(request.pubkey).await;
    let desired = request.account.read();
    let materialization = accessor
        .read(|local| classify_materialization(request.pubkey, local, desired))
        .map_err(errors::ClonerError::from)?
        .transpose()?
        .unwrap_or(Materialization::Apply);
    match materialization {
        Materialization::Apply => Ok(Some(accessor)),
        Materialization::ApplyReadOnly => {
            request.account =
                mem::take(&mut request.account).mode(AccountMode::ReadOnly);
            Ok(Some(accessor))
        }
        Materialization::Satisfied(mode) => {
            accessor.satisfy(mode).await;
            Ok(None)
        }
    }
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
        magicblock_magic_program_api::id(),
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
    accessor: &mut AccountAccessor<'_>,
    request: AccountCloneRequest,
) -> ClonerResult<()> {
    if let Some(authority) = request.delegated_to_other {
        warn!(
            pubkey = %request.pubkey,
            delegated_to = %authority,
            "Cloning account delegated to another validator"
        );
    }
    let actions = match request.post_delegation_mode {
        ClonePostDelegationMode::None => None,
        ClonePostDelegationMode::ExecuteActions(actions) => {
            Some(actions.into_post_finalize())
        }
        ClonePostDelegationMode::RescueUndelegate(source_program) => {
            Some(PostFinalize {
                source_program,
                actions: vec![undelegation_action(engine, request.pubkey)],
            })
        }
    };
    let account = request.account;
    accessor.materialize(account, actions).await.map_err(|err| {
        errors::ClonerError::FailedToCloneRegularAccount(
            request.pubkey,
            Box::new(err.into()),
        )
    })
}

pub(crate) fn resolve_program(
    program: LoadedProgram,
) -> Option<AccountCloneRequest> {
    let program_id = program.program_id;
    if matches!(program.loader_status, LoaderV4Status::Retracted) {
        debug!(%program_id, "Program is retracted on chain");
        return None;
    }

    let owner = match program.loader {
        RemoteProgramLoader::V1 => LOADER_V1,
        RemoteProgramLoader::V2
        | RemoteProgramLoader::V3
        | RemoteProgramLoader::V4 => LOADER_V4,
    };
    let account = AccountBuilder::default()
        .lamports(program.lamports())
        .data(program.program_data)
        .owner(owner)
        .mode(AccountMode::ReadOnly)
        .executable(true)
        .slot(program.remote_slot);

    Some(AccountCloneRequest {
        pubkey: program_id,
        account,
        commit_frequency_ms: None,
        post_delegation_mode: ClonePostDelegationMode::None,
        delegated_to_other: None,
    })
}

pub(crate) async fn clone_program(
    accessor: &mut AccountAccessor<'_>,
    request: AccountCloneRequest,
) -> ClonerResult<()> {
    let program_id = request.pubkey;
    accessor
        .materialize(request.account, None)
        .await
        .map_err(|err| {
            errors::ClonerError::FailedToCloneProgram(
                program_id,
                Box::new(err.into()),
            )
        })
}

pub(crate) async fn evict_account(
    engine: &Engine,
    pubkey: Pubkey,
) -> ClonerResult<()> {
    let Some(mut accessor) = claim_account_eviction(engine, pubkey).await?
    else {
        return Ok(());
    };
    delete_claimed_account(&mut accessor, pubkey).await
}

pub(crate) async fn delete_claimed_account(
    accessor: &mut AccountAccessor<'_>,
    pubkey: Pubkey,
) -> ClonerResult<()> {
    accessor.delete().await.map_err(|err| {
        errors::ClonerError::FailedToEvictAccount(pubkey, Box::new(err.into()))
    })
}

/// Claims an account displaced from Engine recency, unless a later completion
/// retained it again or changed it to an authoritative lifecycle mode. The
/// returned value is projected from the same protected account read.
pub(crate) async fn claim_cached_account_eviction<R>(
    engine: &Engine,
    pubkey: Pubkey,
    inspect: impl Fn(&AccountSharedData) -> R,
) -> ClonerResult<Option<(AccountAccessor<'_>, R)>> {
    let Some((accessor, mode, value)) =
        claim_account_eviction_inner(engine, pubkey, inspect).await?
    else {
        return Ok(None);
    };
    Ok(accessor
        .into_cached_eviction(mode)
        .map(|accessor| (accessor, value)))
}

/// Claims a requested account eviction unless the current state is absent or
/// authoritative.
pub(crate) async fn claim_account_eviction(
    engine: &Engine,
    pubkey: Pubkey,
) -> ClonerResult<Option<AccountAccessor<'_>>> {
    let Some((accessor, mode, ())) =
        claim_account_eviction_inner(engine, pubkey, |_| ()).await?
    else {
        return Ok(None);
    };
    Ok((!mode.authoritative()).then_some(accessor))
}

async fn claim_account_eviction_inner<R>(
    engine: &Engine,
    pubkey: Pubkey,
    inspect: impl Fn(&AccountSharedData) -> R,
) -> ClonerResult<Option<(AccountAccessor<'_>, AccountMode, R)>> {
    let accessor = engine.account(pubkey).await;
    let state = accessor
        .read(|account| (account.mode(), inspect(account)))
        .map_err(|err| {
            errors::ClonerError::FailedToEvictAccount(
                pubkey,
                Box::new(err.into()),
            )
        })?;
    let Some((mode, value)) = state else {
        return Ok(None);
    };
    Ok(Some((accessor, mode, value)))
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
            .materialize(
                AccountBuilder::default()
                    .lamports(1_000_000)
                    .mode(AccountMode::ReadOnly),
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
            .materialize(
                AccountBuilder::default()
                    .lamports(1_000_000)
                    .mode(AccountMode::Ephemeral),
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
