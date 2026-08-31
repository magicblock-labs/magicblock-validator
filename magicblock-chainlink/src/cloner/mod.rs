use engine::{Engine, PostFinalize};
use errors::ClonerResult;
use magicblock_magic_program_api::{
    MAGIC_CONTEXT_PUBKEY,
    args::{
        CommitAndUndelegateArgs, CommitTypeArgs, MagicIntentBundleArgs,
        UndelegateTypeArgs,
    },
    instruction::MagicBlockInstruction,
};
use solana_account::{AccountBuilder, AccountMode};
use solana_instruction::{AccountMeta, Instruction};
use solana_loader_v4_interface::state::LoaderV4Status;
use solana_pubkey::Pubkey;
use tracing::{debug, warn};

use crate::remote_account_provider::program_account::{
    LOADER_V1, LOADER_V4, LoadedProgram, RemoteProgramLoader,
};

pub mod errors;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccountMaterialization {
    Create,
    Update,
}

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
    pub(crate) fn actions(&self) -> Option<&[Instruction]> {
        match self {
            Self::ExecuteActions(actions) => Some(actions.actions()),
            Self::None | Self::RescueUndelegate(_) => None,
        }
    }

    pub(crate) fn has_actions(&self) -> bool {
        self.actions().is_some()
    }

    pub(crate) fn is_rescue_undelegate(&self) -> bool {
        matches!(self, Self::RescueUndelegate(_))
    }

    pub(crate) fn rescue_undelegate(self) -> Option<Self> {
        match self {
            Self::ExecuteActions(actions) => {
                Some(Self::RescueUndelegate(actions.source_program()))
            }
            Self::None | Self::RescueUndelegate(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Proves rescue fallback retains the delegation record owner from the
    /// failed normal post-delegation action path.
    #[test]
    fn rescue_undelegation_preserves_source_program() {
        let source_program = Pubkey::new_unique();
        let mode = ClonePostDelegationMode::from(DelegationActions::new(
            source_program,
            vec![Instruction::new_with_bytes(
                Pubkey::new_unique(),
                &[],
                vec![],
            )],
        ));

        assert_eq!(
            mode.rescue_undelegate(),
            Some(ClonePostDelegationMode::RescueUndelegate(source_program))
        );
    }
}

impl From<Option<DelegationActions>> for ClonePostDelegationMode {
    fn from(actions: Option<DelegationActions>) -> Self {
        actions.map_or(Self::None, Self::ExecuteActions)
    }
}

#[derive(Clone)]
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

fn engine_err(err: impl ToString) -> errors::ClonerError {
    errors::ClonerError::Engine(err.to_string())
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
    request: AccountCloneRequest,
    materialization: AccountMaterialization,
) -> ClonerResult<AccountMode> {
    if let Some(authority) = request.delegated_to_other {
        warn!(
            pubkey = %request.pubkey,
            delegated_to = %authority,
            "Cloning account delegated to another validator"
        );
    }
    if request.post_delegation_mode.is_rescue_undelegate()
        && materialization == AccountMaterialization::Update
    {
        return Err(errors::ClonerError::UndelegationSchedulingUnavailable(
            request.pubkey,
        ));
    }

    let mode = request.account.read().mode();
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
    let result = match materialization {
        AccountMaterialization::Create => {
            engine
                .account(request.pubkey)
                .create(account, actions)
                .await
        }
        AccountMaterialization::Update => {
            engine.account(request.pubkey).update(account).await
        }
    };
    result.map_err(|err| {
        errors::ClonerError::FailedToCloneRegularAccount(
            request.pubkey,
            Box::new(engine_err(err)),
        )
    })?;
    Ok(mode)
}

pub(crate) async fn clone_program(
    engine: &Engine,
    program: LoadedProgram,
    materialization: AccountMaterialization,
) -> ClonerResult<Option<AccountMode>> {
    let program_id = program.program_id;
    if matches!(program.loader_status, LoaderV4Status::Retracted) {
        debug!(%program_id, "Program is retracted on chain");
        return Ok(None);
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

    let result = match materialization {
        AccountMaterialization::Create => {
            engine.account(program_id).create(account, None).await
        }
        AccountMaterialization::Update => {
            engine.account(program_id).update(account).await
        }
    };
    result.map_err(|err| {
        errors::ClonerError::FailedToCloneProgram(
            program_id,
            Box::new(engine_err(err)),
        )
    })?;
    Ok(Some(AccountMode::ReadOnly))
}

pub(crate) async fn evict_account(
    engine: &Engine,
    pubkey: Pubkey,
) -> ClonerResult<()> {
    engine.account(pubkey).delete().await.map_err(|err| {
        errors::ClonerError::FailedToEvictAccount(
            pubkey,
            Box::new(engine_err(err)),
        )
    })
}
