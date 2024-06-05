// FIXME: once we worked this out
#![allow(dead_code)]
#![allow(unused_variables)]

// NOTE: copied from runtime/src/bank.rs:252
use std::sync::{atomic::AtomicU64, Arc};

use sleipnir_accounts_db::accounts::Accounts;

#[derive(Debug)]
pub struct BankRc {
    /// where all the Accounts are stored
    pub accounts: Arc<Accounts>,

    pub(crate) bank_id_generator: Arc<AtomicU64>,
}

#[cfg(RUSTC_WITH_SPECIALIZATION)]
use solana_frozen_abi::abi_example::AbiExample;

#[cfg(RUSTC_WITH_SPECIALIZATION)]
impl AbiExample for BankRc {
    fn example() -> Self {
        BankRc {
            // AbiExample for Accounts is specially implemented to contain a storage example
            accounts: AbiExample::example(),
            bank_id_generator: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl BankRc {
    pub(crate) fn new(accounts: Accounts) -> Self {
        Self {
            accounts: Arc::new(accounts),
            bank_id_generator: Arc::new(AtomicU64::new(0)),
        }
    }
}
