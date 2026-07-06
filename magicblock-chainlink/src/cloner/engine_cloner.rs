//! [`Cloner`] implementation backed by the engine.
//!
//! The engine draws no distinction between a program and any other account:
//! everything is composed through the same CRUD accessor by handing it an
//! [`OwnedAccount`]. `create` decomposes the account into the ordered field
//! patches that reconstruct it, finalizes it — loading it into the program
//! cache when its executable flag is set — and runs any post-delegation actions
//! through `PostFinalize` once the account is live.
//!
//! This subsumes everything the previous cloner drove through the magic
//! program: the buffer/chunked-clone protocol (the engine composes accounts up
//! to 32MB in a single transaction), the loader-v4 deploy dance, and the
//! executable-check toggling around finalization. A program is just an
//! executable account whose data is the bare ELF.

use async_trait::async_trait;
use engine::Engine;
use solana_account::{AccountBuilder, OwnedAccount};
use solana_instruction::Instruction;
use solana_loader_v4_interface::state::LoaderV4Status;
use solana_pubkey::Pubkey;
use tracing::*;

use crate::{
    cloner::{
        AccountCloneRequest, Cloner,
        errors::{ClonerError, ClonerResult},
    },
    remote_account_provider::program_account::{
        LOADER_V1, LOADER_V4, LoadedProgram, RemoteProgramLoader,
    },
};

/// Clones accounts and programs into the validator through the engine.
pub struct ChainlinkCloner {
    engine: Engine,
}

impl ChainlinkCloner {
    pub fn new(engine: Engine) -> Self {
        Self { engine }
    }

    fn engine_err(err: impl ToString) -> ClonerError {
        ClonerError::Engine(err.to_string())
    }
}

#[async_trait]
impl Cloner for ChainlinkCloner {
    /// NOTE: `commit_frequency_ms` is ignored, as it was by the previous cloner
    /// — frequency commits stay disabled pending magicblock-labs/magicblock-validator#625.
    async fn clone_account(
        &self,
        request: AccountCloneRequest,
    ) -> ClonerResult<()> {
        // TODO(phase 4): schedule the undelegation through the engine. Until
        // then this fails closed rather than cloning an account that is
        // supposed to be undelegated and leaving it delegated.
        if request.needs_undelegation {
            return Err(ClonerError::UndelegationSchedulingUnavailable(
                request.pubkey,
            ));
        }

        let actions: Vec<Instruction> = request.delegation_actions.into();
        let actions = (!actions.is_empty()).then_some(actions);

        self.engine
            .account(request.pubkey)
            .create(request.account.owned(), actions)
            .await
            .map_err(|err| {
                ClonerError::FailedToCloneRegularAccount(
                    request.pubkey,
                    Box::new(Self::engine_err(err)),
                )
            })
    }

    async fn clone_program(&self, program: LoadedProgram) -> ClonerResult<()> {
        let program_id = program.program_id;

        // A program retracted on chain is not deployed there, so it is not
        // materialized here either.
        if matches!(program.loader_status, LoaderV4Status::Retracted) {
            debug!(program_id = %program_id, "Program is retracted on chain");
            return Ok(());
        }

        // Programs on the deprecated V1 loader keep it as their owner, since
        // they need assets compiled for it; everything else is owned by V4.
        let owner = match program.loader {
            RemoteProgramLoader::V1 => LOADER_V1,
            RemoteProgramLoader::V2
            | RemoteProgramLoader::V3
            | RemoteProgramLoader::V4 => LOADER_V4,
        };

        // The data field is the bare ELF: the engine carries lamports, slot and
        // the executable flag as account fields, so no loader header, authority
        // or slot is serialized into it.
        let account: OwnedAccount = AccountBuilder::default()
            .lamports(program.lamports())
            .data(program.program_data)
            .owner(owner)
            .executable(true)
            .slot(program.remote_slot)
            .build();

        self.engine
            .account(program_id)
            .create(account, None)
            .await
            .map_err(|err| {
                ClonerError::FailedToCloneProgram(
                    program_id,
                    Box::new(Self::engine_err(err)),
                )
            })
    }

    async fn evict_account(&self, pubkey: Pubkey) -> ClonerResult<()> {
        self.engine.account(pubkey).delete().await.map_err(|err| {
            ClonerError::FailedToEvictAccount(
                pubkey,
                Box::new(Self::engine_err(err)),
            )
        })
    }
}
