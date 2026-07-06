use std::ops::Deref;

use engine::Engine;
use errors::ClonerResult;
use magicblock_magic_program_api::{
    MAGIC_CONTEXT_PUBKEY,
    args::{
        CommitAndUndelegateArgs, CommitTypeArgs, MagicIntentBundleArgs,
        UndelegateTypeArgs,
    },
    instruction::MagicBlockInstruction,
};
use solana_account::{AccountBuilder, AccountMode, OwnedAccount};
use solana_instruction::{AccountMeta, Instruction};
use solana_loader_v4_interface::state::LoaderV4Status;
use solana_pubkey::Pubkey;
use tracing::debug;

use crate::remote_account_provider::program_account::{
    LOADER_V1, LOADER_V4, LoadedProgram, RemoteProgramLoader,
};

pub mod errors;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccountMaterialization {
    Create,
    Update,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DelegationActions(Vec<Instruction>);

impl DelegationActions {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<Vec<Instruction>> for DelegationActions {
    fn from(value: Vec<Instruction>) -> Self {
        Self(value)
    }
}

impl From<DelegationActions> for Vec<Instruction> {
    fn from(value: DelegationActions) -> Self {
        value.0
    }
}

impl IntoIterator for DelegationActions {
    type Item = Instruction;
    type IntoIter = std::vec::IntoIter<Instruction>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl Deref for DelegationActions {
    type Target = [Instruction];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Clone)]
pub struct AccountCloneRequest {
    pub pubkey: Pubkey,
    pub account: AccountBuilder,
    pub commit_frequency_ms: Option<u64>,
    pub delegation_actions: DelegationActions,
    /// If the account is delegated to another validator,
    /// this contains that validator's pubkey. None if account is not
    /// delegated to another validator.
    pub delegated_to_other: Option<Pubkey>,
    /// Account that need to be undelegated (e.g. due to AML risk) after cloning.
    /// Is only true if there are actions with risky signers.
    pub needs_undelegation: bool,
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
    if request.needs_undelegation
        && materialization == AccountMaterialization::Update
    {
        return Err(errors::ClonerError::UndelegationSchedulingUnavailable(
            request.pubkey,
        ));
    }

    let mode = request.account.read().mode();
    let actions = if request.needs_undelegation {
        vec![undelegation_action(engine, request.pubkey)]
    } else {
        request.delegation_actions.into()
    };
    let account: OwnedAccount = request.account.build();
    let result = match materialization {
        AccountMaterialization::Create => {
            let actions = (!actions.is_empty()).then_some(actions);
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
    let account: OwnedAccount = AccountBuilder::default()
        .lamports(program.lamports())
        .data(program.program_data)
        .owner(owner)
        .mode(AccountMode::ReadOnly)
        .executable(true)
        .slot(program.remote_slot)
        .build();

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
