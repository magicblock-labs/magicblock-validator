//! Shared construction of the exact engine image used by MBV roles.

mod accounts;

use std::{collections::HashMap, fs, path::PathBuf};

use keeper::builder::KeeperBuilder;
use magicblock_config::config::{EngineConfig, LoadableProgram};
use magicblock_program::magicblock_processor::{
    CallbackEntrypoint, CrankEntrypoint, Entrypoint, EphemeralSystemEntrypoint,
    OutboxIntentEntrypoint,
};
use solana_program_runtime::{
    invoke_context::BuiltinFunctionWithContext,
    solana_sbpf::program::BuiltinFunctionDefinition,
};
use solana_pubkey::Pubkey;
use solana_rent::Rent;

/// Builds the Keeper image shared by leaders and verifiers.
pub fn keeper_builder<R>(
    engine: &EngineConfig<R>,
    programs: &[LoadableProgram],
) -> Result<KeeperBuilder, Error> {
    let programs = load_programs(programs)?;
    Ok(KeeperBuilder {
        authority: engine.authority.clone(),
        accountsdb: engine.accountsdb.clone(),
        ledger: engine.ledger.clone(),
        blockstore: engine.blockstore,
        builtins: builtins(),
        accounts: accounts::initial_accounts(&programs),
        programs,
        rent: Rent::default(),
    })
}

fn load_programs(
    programs: &[LoadableProgram],
) -> Result<HashMap<Pubkey, Vec<u8>>, Error> {
    programs
        .iter()
        .map(|program| {
            fs::read(&program.path)
                .map(|elf| (program.id.0, elf))
                .map_err(|source| Error::Program {
                    id: program.id.0,
                    path: program.path.clone(),
                    source,
                })
        })
        .collect()
}

fn builtins() -> HashMap<Pubkey, BuiltinFunctionWithContext> {
    let mut builtins = HashMap::<Pubkey, BuiltinFunctionWithContext>::new();
    builtins.insert(
        magicblock_program::ID,
        (Entrypoint::vm, Entrypoint::codegen),
    );
    builtins.insert(
        magicblock_program::CRANK_PROGRAM_ID,
        (CrankEntrypoint::vm, CrankEntrypoint::codegen),
    );
    builtins.insert(
        magicblock_program::CALLBACK_PROGRAM_ID,
        (CallbackEntrypoint::vm, CallbackEntrypoint::codegen),
    );
    builtins.insert(
        magicblock_program::EPHEMERAL_SYSTEM_PROGRAM_ID,
        (
            EphemeralSystemEntrypoint::vm,
            EphemeralSystemEntrypoint::codegen,
        ),
    );
    builtins.insert(
        magicblock_program::OUTBOX_INTENT_PROGRAM_ID,
        (OutboxIntentEntrypoint::vm, OutboxIntentEntrypoint::codegen),
    );
    builtins
}

/// Runtime image construction failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read startup program {id} at {}: {source}", path.display())]
    Program {
        id: Pubkey,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
