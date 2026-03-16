use core::fmt;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use magicblock_core::{intent::CommittedAccount, traits::MagicSys};
use magicblock_magic_program_api::{id, EPHEMERAL_VAULT_PUBKEY};
use solana_account::AccountSharedData;
use solana_instruction::{error::InstructionError, AccountMeta};
use solana_program_runtime::invoke_context::mock_process_instruction;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;

use self::magicblock_processor::Entrypoint;
use super::*;
use crate::validator;

pub const AUTHORITY_BALANCE: u64 = u64::MAX / 2;
pub const COUNTER_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("2jQZbSfAfqT5nZHGrLpDG2vXuEGtTgZYnNy7AZEjMCYz");

pub fn ensure_started_validator(
    map: &mut HashMap<Pubkey, AccountSharedData>,
    nonce: Option<u64>,
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

    let stub = Arc::new(MagicSysStub::with_nonce(nonce.unwrap_or(0)));
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
        Vec::new(),
        instruction_data,
        transaction_accounts,
        instruction_accounts,
        expected_result,
        Entrypoint::vm,
        |_invoke_context| {},
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

pub struct MagicSysStub {
    id: u64,
    nonce: u64,
}

impl Default for MagicSysStub {
    fn default() -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);

        Self {
            id: ID.fetch_add(1, Ordering::Relaxed),
            nonce: 0,
        }
    }
}

impl MagicSysStub {
    pub fn with_nonce(nonce: u64) -> Self {
        Self {
            nonce,
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
        Ok(commits.iter().map(|c| (c.pubkey, self.nonce)).collect())
    }
}
