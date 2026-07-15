use core::fmt;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use magicblock_core::{
    intent::{types::CommittedAccount, MagicIntentBundle},
    traits::MagicSys,
};
use magicblock_magic_program_api::{
    id, EPHEMERAL_SYSTEM_PROGRAM_ID, EPHEMERAL_VAULT_PUBKEY,
    OUTBOX_INTENT_PROGRAM_ID,
};
use solana_account::AccountSharedData;
use solana_instruction::{error::InstructionError, AccountMeta};
use solana_program_runtime::{
    invoke_context::{mock_process_instruction, InvokeContext},
    loaded_programs::ProgramCacheEntry,
};
use solana_pubkey::Pubkey;
use solana_sdk_ids::{native_loader, system_program};

use self::magicblock_processor::{
    Entrypoint, EphemeralSystemEntrypoint, OutboxIntentEntrypoint,
};
use super::*;
use crate::validator;

/// Registers the sibling builtins CPI'd into from within a test so
/// `native_invoke` can resolve them — `mock_process_instruction` only wires
/// up the single entrypoint under test.
fn register_sibling_builtins(invoke_context: &mut InvokeContext) {
    invoke_context.program_cache_for_tx_batch.replenish(
        OUTBOX_INTENT_PROGRAM_ID,
        Arc::new(ProgramCacheEntry::new_builtin(
            0,
            0,
            OutboxIntentEntrypoint::vm,
        )),
    );
    invoke_context.program_cache_for_tx_batch.replenish(
        EPHEMERAL_SYSTEM_PROGRAM_ID,
        Arc::new(ProgramCacheEntry::new_builtin(
            0,
            0,
            EphemeralSystemEntrypoint::vm,
        )),
    );
}

pub const AUTHORITY_BALANCE: u64 = u64::MAX / 2;
pub const COUNTER_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("2jQZbSfAfqT5nZHGrLpDG2vXuEGtTgZYnNy7AZEjMCYz");

pub fn ensure_started_validator(
    map: &mut HashMap<Pubkey, AccountSharedData>,
    nonces: Option<StubNonces>,
) {
    validator::generate_validator_authority_if_needed();
    let validator_authority_id = validator::validator_authority_id();
    map.entry(validator_authority_id).or_insert_with(|| {
        AccountSharedData::new(AUTHORITY_BALANCE, 0, &system_program::id())
    });

    // Ensure ephemeral vault account exists
    map.entry(EPHEMERAL_VAULT_PUBKEY).or_insert_with(|| {
        let mut vault = AccountSharedData::new(0, 0, &id());
        vault.set_ephemeral(true);
        vault
    });

    // Ensure the sibling builtin programs CPI'd into from the main
    // entrypoint are resolvable as accounts (mock_process_instruction only
    // auto-injects the entrypoint under test, not its CPI targets)
    map.entry(EPHEMERAL_SYSTEM_PROGRAM_ID)
        .or_insert_with(|| AccountSharedData::new(0, 0, &native_loader::id()));
    map.entry(OUTBOX_INTENT_PROGRAM_ID)
        .or_insert_with(|| AccountSharedData::new(0, 0, &native_loader::id()));

    let stub = Arc::new(match nonces {
        None => MagicSysStub::default(),
        Some(n) => MagicSysStub {
            nonces: n,
            ..MagicSysStub::default()
        },
    });
    init_magic_sys(stub);

    // Ensure validator is in Primary mode (ledger replay complete)
    magicblock_core::coordination_mode::switch_to_primary_mode();
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
        Entrypoint::vm,
        register_sibling_builtins,
        |invoke_context| {
            logs = invoke_context
                .get_log_collector()
                .map(|collector| {
                    collector.borrow().get_recorded_content().to_vec()
                })
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
        OutboxIntentEntrypoint::vm,
        register_sibling_builtins,
        |invoke_context| {
            logs = invoke_context
                .get_log_collector()
                .map(|collector| {
                    collector.borrow().get_recorded_content().to_vec()
                })
                .unwrap_or_default();
        },
    );

    (accounts, logs)
}

pub enum StubNonces {
    Global(u64),
    PerAccount(HashMap<Pubkey, u64>),
}

pub struct MagicSysStub {
    id: u64,
    nonces: StubNonces,
}

impl Default for MagicSysStub {
    fn default() -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);
        Self {
            id: ID.fetch_add(1, Ordering::Relaxed),
            nonces: StubNonces::Global(0),
        }
    }
}

impl MagicSysStub {
    pub fn with_nonce(nonce: u64) -> Self {
        Self {
            nonces: StubNonces::Global(nonce),
            ..Self::default()
        }
    }

    pub fn with_per_account_nonces(nonces: HashMap<Pubkey, u64>) -> Self {
        Self {
            nonces: StubNonces::PerAccount(nonces),
            ..Self::default()
        }
    }
}

impl fmt::Display for MagicSysStub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MagicSysStub({})", self.id)
    }
}

impl MagicSys for MagicSysStub {
    fn fetch_current_commit_nonces(
        &self,
        commits: &[CommittedAccount],
    ) -> Result<HashMap<Pubkey, u64>, InstructionError> {
        match &self.nonces {
            StubNonces::Global(nonce) => {
                Ok(commits.iter().map(|c| (c.pubkey, *nonce)).collect())
            }
            StubNonces::PerAccount(nonces) => commits
                .iter()
                .map(|c| {
                    nonces.get(&c.pubkey).copied().map(|n| (c.pubkey, n)).ok_or(
                        InstructionError::Custom(
                            crate::magic_sys::MISSING_COMMIT_NONCE_ERR,
                        ),
                    )
                })
                .collect(),
        }
    }

    fn validate_intent_size(
        &self,
        _intent: &MagicIntentBundle,
    ) -> Result<(), InstructionError> {
        Ok(())
    }
}
