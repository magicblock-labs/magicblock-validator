use std::{collections::HashMap, sync::Arc};

use magic_program_api::{
    MAGIC_ACCOUNT_PROGRAM_ID, MAGIC_ACCOUNT_VAULT_PUBKEY, OUTBOX_INTENT_PROGRAM_ID, id,
};
use solana_account::{AccountBuilder, AccountMode, AccountSharedData};
use solana_instruction::{AccountMeta, error::InstructionError};
use solana_program_runtime::{
    invoke_context::{InvokeContext, mock_process_instruction},
    loaded_programs::ProgramCacheEntry,
    solana_sbpf::program::BuiltinFunctionDefinition,
};
use solana_pubkey::Pubkey;
use solana_sdk_ids::{native_loader, system_program};

use self::magicblock_processor::{Entrypoint, MagicAccountEntrypoint, OutboxIntentEntrypoint};
use super::*;
use crate::validator;

/// Registers the sibling builtins CPI'd into from within a test so
/// `native_invoke` can resolve them — `mock_process_instruction` only wires
/// up the single entrypoint under test.
fn register_sibling_builtins(invoke_context: &mut InvokeContext<'_, '_>) {
    invoke_context.program_cache_for_tx_batch.replenish(
        OUTBOX_INTENT_PROGRAM_ID,
        Arc::new(ProgramCacheEntry::new_builtin((
            OutboxIntentEntrypoint::vm,
            OutboxIntentEntrypoint::codegen,
        ))),
    );
    invoke_context.program_cache_for_tx_batch.replenish(
        MAGIC_ACCOUNT_PROGRAM_ID,
        Arc::new(ProgramCacheEntry::new_builtin((
            MagicAccountEntrypoint::vm,
            MagicAccountEntrypoint::codegen,
        ))),
    );
}

pub const AUTHORITY_BALANCE: u64 = u64::MAX / 2;
pub const COUNTER_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("2jQZbSfAfqT5nZHGrLpDG2vXuEGtTgZYnNy7AZEjMCYz");

/// Initializes only the current thread's public identity, preserving any
/// authority already supplied by Engine or the test.
pub(crate) fn ensure_authority() -> Pubkey {
    let mut authority = validator::authority();
    if authority == Pubkey::default() {
        authority = Pubkey::new_unique();
        nucleus::tls::AUTHORITY.set(authority);
    }
    authority
}

pub fn ensure_started_validator(
    map: &mut HashMap<Pubkey, AccountSharedData>,
    nonces: Option<StubNonces>,
) {
    let validator_authority_id = ensure_authority();
    map.entry(validator_authority_id)
        .or_insert_with(|| AccountSharedData::new(AUTHORITY_BALANCE, 0, &system_program::id()));

    // Ensure Magic account vault exists
    map.entry(MAGIC_ACCOUNT_VAULT_PUBKEY).or_insert_with(|| {
        AccountBuilder::from(AccountSharedData::new(0, 0, &id()))
            .mode(AccountMode::Magic)
            .build()
    });

    // Ensure the sibling builtin programs CPI'd into from the main
    // entrypoint are resolvable as accounts (mock_process_instruction only
    // auto-injects the entrypoint under test, not its CPI targets)
    map.entry(MAGIC_ACCOUNT_PROGRAM_ID)
        .or_insert_with(|| AccountSharedData::new(0, 0, &native_loader::id()));
    map.entry(OUTBOX_INTENT_PROGRAM_ID)
        .or_insert_with(|| AccountSharedData::new(0, 0, &native_loader::id()));

    if let Some(nonces) = nonces {
        for (pubkey, account) in map.iter_mut() {
            let nonce = match &nonces {
                StubNonces::Global(nonce) => *nonce,
                StubNonces::PerAccount(nonces) => nonces.get(pubkey).copied().unwrap_or_default(),
            };
            assert!(nonce <= u64::from(u32::MAX), "stub nonce fits u32");
            *account =
                AccountBuilder::from(std::mem::take(account)).commit_seq(nonce as u32).build();
        }
    }
}

pub fn process_instruction(
    instruction_data: &[u8],
    transaction_accounts: Vec<(Pubkey, AccountSharedData)>,
    instruction_accounts: Vec<AccountMeta>,
    expected_result: Result<(), InstructionError>,
) -> Vec<AccountSharedData> {
    process_instruction_with_logs(
        instruction_data,
        transaction_accounts,
        instruction_accounts,
        expected_result,
    )
    .0
}

pub fn process_instruction_with_logs(
    instruction_data: &[u8],
    transaction_accounts: Vec<(Pubkey, AccountSharedData)>,
    instruction_accounts: Vec<AccountMeta>,
    expected_result: Result<(), InstructionError>,
) -> (Vec<AccountSharedData>, Vec<String>) {
    let mut logs = Vec::new();
    let accounts = mock_process_instruction(
        &crate::id(),
        None,
        instruction_data,
        transaction_accounts,
        instruction_accounts,
        expected_result,
        (Entrypoint::vm, Entrypoint::codegen),
        register_sibling_builtins,
        |invoke_context| {
            logs = invoke_context
                .get_log_collector()
                .map(|collector| collector.borrow().get_recorded_content().to_vec())
                .unwrap_or_default();
        },
    );

    (accounts, logs)
}

pub fn process_outbox_intent_instruction(
    instruction_data: &[u8],
    transaction_accounts: Vec<(Pubkey, AccountSharedData)>,
    instruction_accounts: Vec<AccountMeta>,
    expected_result: Result<(), InstructionError>,
) -> Vec<AccountSharedData> {
    process_outbox_intent_instruction_with_logs(
        instruction_data,
        transaction_accounts,
        instruction_accounts,
        expected_result,
    )
    .0
}

pub fn process_outbox_intent_instruction_with_logs(
    instruction_data: &[u8],
    transaction_accounts: Vec<(Pubkey, AccountSharedData)>,
    instruction_accounts: Vec<AccountMeta>,
    expected_result: Result<(), InstructionError>,
) -> (Vec<AccountSharedData>, Vec<String>) {
    let mut logs = Vec::new();
    let accounts = mock_process_instruction(
        &OUTBOX_INTENT_PROGRAM_ID,
        None,
        instruction_data,
        transaction_accounts,
        instruction_accounts,
        expected_result,
        (OutboxIntentEntrypoint::vm, OutboxIntentEntrypoint::codegen),
        register_sibling_builtins,
        |invoke_context| {
            logs = invoke_context
                .get_log_collector()
                .map(|collector| collector.borrow().get_recorded_content().to_vec())
                .unwrap_or_default();
        },
    );

    (accounts, logs)
}

pub enum StubNonces {
    Global(u64),
    PerAccount(HashMap<Pubkey, u64>),
}
