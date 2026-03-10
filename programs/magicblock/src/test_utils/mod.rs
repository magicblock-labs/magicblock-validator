use core::fmt;
use std::{
    collections::HashMap,
    error::Error,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use magicblock_core::{intent::CommittedAccount, traits::MagicSys};
use magicblock_magic_program_api::{id, EPHEMERAL_VAULT_PUBKEY};
use solana_account::AccountSharedData;
use solana_instruction::{error::InstructionError, AccountMeta};
use solana_log_collector::log::debug;
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

    let stub = Arc::new(match nonces {
        None => MagicSysStub::default(),
        Some(n) => MagicSysStub {
            nonces: n,
            ..MagicSysStub::default()
        },
    });
    init_magic_sys(stub);

    validator::ensure_started_up();
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
    fn persist(&self, id: u64, data: Vec<u8>) -> Result<(), Box<dyn Error>> {
        debug!("Persisting data for id '{}' with len {}", id, data.len());
        Ok(())
    }

    fn load(&self, _id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
        Err("Loading from ledger not supported in tests".into())
    }

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
}
