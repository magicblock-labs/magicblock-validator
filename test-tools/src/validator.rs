use std::{
    error::Error,
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use log::*;
use magicblock_accounts_db::AccountsDb;
use magicblock_core::traits::PersistsAccountModData;
use magicblock_program::{init_persister, validator};
use solana_sdk::native_token::LAMPORTS_PER_SOL;

use crate::account::fund_account;

fn ensure_funded_validator(accountsdb: &AccountsDb) {
    validator::generate_validator_authority_if_needed();
    fund_account(
        accountsdb,
        &validator::validator_authority_id(),
        LAMPORTS_PER_SOL * 1_000,
    );
}

// -----------------
// Persister
// -----------------
pub struct PersisterStub {
    id: u64,
}

impl Default for PersisterStub {
    fn default() -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);

        Self {
            id: ID.fetch_add(1, Ordering::Relaxed),
        }
    }
}

impl fmt::Display for PersisterStub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PersisterStub({})", self.id)
    }
}

impl PersistsAccountModData for PersisterStub {
    fn persist(&self, id: u64, data: Vec<u8>) -> Result<(), Box<dyn Error>> {
        debug!("Persisting data for id '{}' with len {}", id, data.len());
        Ok(())
    }

    fn load(&self, _id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
        Err("Loading from ledger not supported in tests".into())
    }
}

pub fn init_started_validator(accountsdb: &AccountsDb) {
    ensure_funded_validator(accountsdb);
    let stub = Arc::new(PersisterStub::default());
    init_persister(stub);
    validator::ensure_started_up();
}
