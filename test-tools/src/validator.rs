use crate::account::fund_account;
use log::*;
use sleipnir_bank::bank::Bank;
use sleipnir_core::traits::PersistsAccountModData;
use sleipnir_program::{init_persister, validator};
use solana_sdk::native_token::LAMPORTS_PER_SOL;
use std::error::Error;
use std::sync::Arc;

fn ensure_funded_validator(bank: &Bank) {
    validator::generate_validator_authority_if_needed();
    fund_account(
        bank,
        &validator::validator_authority_id(),
        LAMPORTS_PER_SOL * 1_000,
    );
}

struct PersisterStub;
impl PersistsAccountModData for PersisterStub {
    fn persist(&self, id: u64, data: Vec<u8>) -> Result<(), Box<dyn Error>> {
        debug!("Persisting data for id '{}' with len {}", id, data.len());
        Ok(())
    }

    fn load(&self, _id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
        Err("Loading from ledger not supported in tests".into())
    }
}

pub fn init_persister_stub() {
    init_persister(Arc::new(PersisterStub));
}

pub fn init_started_validator(bank: &Bank) {
    ensure_funded_validator(bank);
    init_persister_stub();
    validator::finished_starting_up();
}
