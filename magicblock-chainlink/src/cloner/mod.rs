use std::ops::Deref;

use engine::Engine;
use errors::ClonerResult;
use solana_account::{AccountBuilder, AccountSharedData, OwnedAccount};
use solana_instruction::Instruction;
use solana_loader_v4_interface::state::LoaderV4Status;
use solana_pubkey::Pubkey;
use tracing::debug;

use crate::remote_account_provider::program_account::{
    LOADER_V1, LOADER_V4, LoadedProgram, RemoteProgramLoader,
};

pub mod errors;

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
    pub account: AccountSharedData,
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

pub(crate) async fn clone_account(
    engine: &Engine,
    request: AccountCloneRequest,
) -> ClonerResult<()> {
    if request.needs_undelegation {
        return Err(errors::ClonerError::UndelegationSchedulingUnavailable(
            request.pubkey,
        ));
    }

    let actions: Vec<Instruction> = request.delegation_actions.into();
    let actions = (!actions.is_empty()).then_some(actions);
    engine
        .account(request.pubkey)
        .create(request.account.owned(), actions)
        .await
        .map_err(|err| {
            errors::ClonerError::FailedToCloneRegularAccount(
                request.pubkey,
                Box::new(engine_err(err)),
            )
        })
}

pub(crate) async fn clone_program(
    engine: &Engine,
    program: LoadedProgram,
) -> ClonerResult<()> {
    let program_id = program.program_id;
    if matches!(program.loader_status, LoaderV4Status::Retracted) {
        debug!(%program_id, "Program is retracted on chain");
        return Ok(());
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
        .executable(true)
        .slot(program.remote_slot)
        .build();

    engine
        .account(program_id)
        .create(account, None)
        .await
        .map_err(|err| {
            errors::ClonerError::FailedToCloneProgram(
                program_id,
                Box::new(engine_err(err)),
            )
        })
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
